//! Account status and usage for the status bar, Settings and the Usage panel
//! (PANEL-064..066, PANEL-143..149, PANEL-180..190, SHELL-221..224).
//!
//! One [`UsageService`] per process feeds both modules. `Accounts.subscribe` is
//! cache-only: it reads `codex-accounts.json` and settings, and never starts
//! Orca. `Accounts.refresh` runs Orca's discovery once (single-flight).
//! `Usage.subscribe` reads Codex usage per account home with today's backoff
//! (`codex_refresh_interval`), one read in flight per home, only while a
//! subscriber exists, on threads of its own; every read also feeds subscription
//! balancing. Claude figures are the file-only status-line caches of the shells
//! the native session inventory lists. Visible Usage panels register their project
//! with the same service: Grok session tokens and cost are sampled once per
//! process for those projects, using the engine's bounded, shared usage cache.
//!
//! The 15 min / 60 s intervals and the 15-minute staleness are evaluated on the
//! service's own thread: it waits for the earliest deadline while subscribed,
//! and blocks with no subscriber. Home changes (store, sessions, settings and
//! `usage/`) wake it through the process service's change observers.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::FutureExt;
use nsh_core::prelude::*;
use riwork_core::{
    codex_accounts::{self, AccountsSnapshot, CodexAccount, SYSTEM_DEFAULT_ID},
    settings::SettingsStore,
};
use riwork_engine::usage::{self as engine_usage, ProviderUsage};
use serde::{Deserialize, Serialize};

use crate::{
    dto::{SessionsSnapshot, ShellSessionDto},
    hub::{Hub, Snapshot, TopicSubscription},
    service::{self, blocking},
};

pub const SCHEMA: u32 = 1;
/// Figures older than this may have moved (`account_status.rs` `stale`).
pub const STALE_AFTER_SECS: u64 = 15 * 60;
pub const SYSTEM_LABEL: &str = "System default";
/// PANEL-147: choosing an unavailable account changes nothing.
pub const UNAVAILABLE_ACCOUNT: &str =
    "This account is unavailable. Refresh accounts or choose another.";
/// What the fixture backend reports for an account it has no figures for.
pub const FIXTURE_UNAVAILABLE: &str = "Account usage is unavailable";
const CACHE_NAME: &str = "codex-accounts.json";
const FIXTURE_NAME: &str = "usage-fixture.json";

// ---- Payloads -----------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindowDto {
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<u64>,
    pub window_minutes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageDto {
    pub provider: String,
    pub windows: Vec<UsageWindowDto>,
    pub updated_at_unix: u64,
    /// What the provider calls the account (Codex: email and plan).
    pub account_label: Option<String>,
    pub context_used_percent: Option<f64>,
    pub session_cost_usd: Option<f64>,
}

impl From<&ProviderUsage> for ProviderUsageDto {
    fn from(usage: &ProviderUsage) -> Self {
        Self {
            provider: usage.provider.clone(),
            windows: usage
                .windows
                .iter()
                .map(|window| UsageWindowDto {
                    label: window.label.clone(),
                    used_percent: window.used_percent,
                    resets_at: window.resets_at,
                    window_minutes: window.window_minutes,
                })
                .collect(),
            updated_at_unix: usage.updated_at_unix,
            account_label: usage.account_label.clone(),
            context_used_percent: usage.context_used_percent,
            session_cost_usd: usage.session_cost_usd,
        }
    }
}

impl ProviderUsageDto {
    /// A cached Claude window describes the interval ending at `resets_at`.
    /// Once that interval ends its next utilization is unknown, not 0%. Keep
    /// the report's context/cost and original timestamp, but stop presenting
    /// the previous allowance as capacity available now.
    fn claude_at(usage: &ProviderUsage, now: u64) -> Self {
        let mut value = Self::from(usage);
        value.windows.retain(|window| {
            window.resets_at.is_none_or(|reset| reset > now)
                && window.used_percent.is_finite()
                && window.used_percent >= 0.0
                && (window.label == "Spend limit" || window.used_percent <= 100.0)
        });
        value
    }
}

/// One Codex account home whose usage is read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct CodexUsageDto {
    /// Absolute home; matches `CodexAccountDto.homePath` and shells' `codexHome`.
    pub home: String,
    pub account_id: Option<String>,
    pub label: String,
    pub usage: Option<ProviderUsageDto>,
    /// The last read failed (over `usage` when both are present).
    pub error: Option<String>,
    /// A read is in flight.
    pub pending: bool,
    pub last_attempt: Option<u64>,
    /// Consecutive failed reads; each doubles the wait (at most 2^6).
    pub failures: u32,
    /// `usage` is older than 15 minutes at `now`.
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeUsageDto {
    pub shell_id: String,
    pub project_id: Option<String>,
    pub usage: Option<ProviderUsageDto>,
    pub error: Option<String>,
    /// Claude has not reported yet (it does after its first response).
    pub waiting: bool,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct GrokTokenCountsDto {
    pub input: u64,
    pub output: u64,
    pub cached_read: u64,
    pub cache_creation: u64,
    pub reasoning: u64,
    pub total: u64,
}
impl From<&engine_usage::TokenCounts> for GrokTokenCountsDto {
    fn from(value: &engine_usage::TokenCounts) -> Self {
        Self { input: value.input, output: value.output, cached_read: value.cached_read,
            cache_creation: value.cache_creation, reasoning: value.reasoning, total: value.total }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct GrokModelUsageDto {
    pub model: String,
    pub model_calls: u64,
    pub tokens: GrokTokenCountsDto,
    pub cost_usd: Option<f64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct GrokSessionUsageDto {
    pub session_id: String,
    pub updated_at: Option<String>,
    pub updated_at_unix: Option<u64>,
    pub primary_model: Option<String>,
    pub turns: u64,
    pub model_calls: u64,
    pub tokens: GrokTokenCountsDto,
    pub cost_usd: Option<f64>,
    pub models: Vec<GrokModelUsageDto>,
}
impl From<&engine_usage::SessionUsage> for GrokSessionUsageDto {
    fn from(value: &engine_usage::SessionUsage) -> Self {
        Self {
            session_id: value.session_id.clone(), updated_at: value.updated_at.clone(),
            updated_at_unix: value.updated_at_unix, primary_model: value.primary_model.clone(),
            turns: value.turns, model_calls: value.model_calls, tokens: (&value.tokens).into(),
            cost_usd: value.cost_usd,
            models: value.models.iter().map(|model| GrokModelUsageDto {
                model: model.model.clone(), model_calls: model.model_calls,
                tokens: (&model.tokens).into(), cost_usd: model.cost_usd,
            }).collect(),
        }
    }
}
/// Grok's local session figures; account allowance is only available in its own UI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct GrokUsageDto {
    pub shell_id: String,
    pub project_id: Option<String>,
    pub label: String,
    pub usage: Option<GrokSessionUsageDto>,
    pub error: Option<String>,
    pub stale: bool,
    pub pending: bool,
    pub fetched_at_unix: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct UsageDto {
    pub schema: u32,
    /// The service clock (unix seconds) the stale flags were evaluated at.
    pub now: u64,
    pub codex: Vec<CodexUsageDto>,
    pub claude: Vec<ClaudeUsageDto>,
    pub grok: Vec<GrokUsageDto>,
    /// A provider read is in flight (the Usage panel's "Refreshing…").
    pub refreshing: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct CodexAccountDto {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    /// For display, the user's home shortened to `~` (`display_home`).
    pub home: String,
    pub home_path: String,
    pub system: bool,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    /// Saved in Orca (every account but the system default).
    pub saved: bool,
    /// Orca's own current account.
    pub orca_active: bool,
    /// PANEL-146: the unavailable reason, "Current Codex profile · {~home}",
    /// "Current Orca account" or "Saved in Orca".
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct AccountsDto {
    pub schema: u32,
    /// The system default first, then the saved accounts, unavailable included.
    /// Before any list was read, the system default alone.
    pub accounts: Vec<CodexAccountDto>,
    /// The app's default (`selected_codex_account`); null is the system default.
    pub selected_id: Option<String>,
    /// PANEL-149: the selected saved account is not an available listed one.
    pub selected_unavailable: bool,
    pub balancing: bool,
    /// A list was read: the saved cache, or a discovery in this process.
    pub listed: bool,
    /// The last discovery failed and fell back to the saved list.
    pub from_cache: bool,
    pub error: Option<String>,
    pub refreshing: bool,
}

// ---- Backends -----------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodexSettings {
    pub selected: Option<String>,
    pub balancing: bool,
}

/// An account home the service reads Codex usage for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexTarget {
    pub home: PathBuf,
    pub account_id: Option<String>,
    pub label: String,
}

/// Synchronous seam; every call runs on a service or blocking-pool thread.
/// The production reads that start processes are marked; tests use fakes.
pub trait UsageBackend: Send + Sync + 'static {
    /// SPAWNS `codex app-server` in production (10 s timeout).
    fn read_codex(&self, target: &CodexTarget) -> Result<ProviderUsage, String>;
    /// After every Codex read: feeds subscription balancing (a file write).
    fn record_codex(&self, _home: &Path, _usage: Option<&ProviderUsage>) {}
    /// File-only status-line cache of one Claude shell.
    fn read_claude(&self, shell_id: &str) -> Result<Option<ProviderUsage>, String>;
    /// The saved account list, or None when none was ever saved. Never spawns.
    fn cached_accounts(&self) -> Option<AccountsSnapshot>;
    /// Bounded reads on a worker. Fixture backends never launch a harness.
    fn read_grok(&self, ids: &[String], _previous: &BTreeMap<String, engine_usage::GrokTabUsage>, _force: bool) -> BTreeMap<String, engine_usage::GrokTabUsage> {
        ids.iter().map(|id| (id.clone(), engine_usage::GrokTabUsage::unavailable("Session usage unavailable"))).collect()
    }
    /// SPAWNS Orca's CLI in production (5 s timeout) and rewrites the cache.
    fn discover_accounts(&self) -> AccountsSnapshot;
    /// The system default before any list was read (`default_codex_home`).
    fn system_account(&self) -> Result<CodexAccount, String> {
        system_account()
    }
    fn codex_settings(&self) -> Result<CodexSettings, String>;
    fn save_selected_account(&self, selected: Option<String>) -> Result<CodexSettings, String>;
}

pub fn system_account() -> Result<CodexAccount, String> {
    codex_accounts::default_codex_home().map(|home| CodexAccount {
        id: SYSTEM_DEFAULT_ID.into(),
        label: SYSTEM_LABEL.into(),
        email: None,
        home,
        available: true,
        unavailable_reason: None,
        is_system_default: true,
    })
}

/// The saved list with Orca's active account; file-only.
pub fn read_saved_accounts(state_home: &Path) -> Option<AccountsSnapshot> {
    let accounts = codex_accounts::cached_accounts(state_home).ok()?;
    let source_active_id = fs::read(state_home.join(CACHE_NAME))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.get("source_active_id")?.as_str().map(str::to_owned))
        .filter(|id| accounts.iter().any(|account| &account.id == id));
    Some(AccountsSnapshot {
        accounts,
        error: None,
        source_active_id,
        from_cache: false,
    })
}

fn settings_of(store: &SettingsStore) -> Result<CodexSettings, String> {
    store
        .load()
        .map(|settings| CodexSettings {
            selected: settings.selected_codex_account,
            balancing: settings.codex_subscription_balancing,
        })
        .map_err(service::io_error)
}

fn save_selected(store: &SettingsStore, selected: Option<String>) -> Result<CodexSettings, String> {
    store
        .update_selected(&["selected_codex_account"], move |settings| {
            settings.selected_codex_account = selected
        })
        .map(|settings| CodexSettings {
            selected: settings.selected_codex_account,
            balancing: settings.codex_subscription_balancing,
        })
        .map_err(service::io_error)
}

type Stamp = Option<(Option<SystemTime>, u64)>;

fn stamp(path: &Path) -> Stamp {
    fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.modified().ok(), metadata.len()))
}

/// riwork-core / riwork-engine, unchanged.
pub struct CoreUsageBackend {
    home: PathBuf,
    settings: SettingsStore,
    saved: Mutex<Option<(Stamp, Option<AccountsSnapshot>)>>,
}

impl CoreUsageBackend {
    pub fn new(home: PathBuf) -> Result<Self, String> {
        Ok(Self {
            settings: SettingsStore::open(home.clone())?,
            home,
            saved: Mutex::new(None),
        })
    }
}

impl UsageBackend for CoreUsageBackend {
    fn read_codex(&self, target: &CodexTarget) -> Result<ProviderUsage, String> {
        engine_usage::read_codex_usage_at(&target.home)
    }
    fn record_codex(&self, home: &Path, usage: Option<&ProviderUsage>) {
        let _ = riwork_engine::codex_balancing::record_result(&self.home, home, usage);
    }
    fn read_claude(&self, shell_id: &str) -> Result<Option<ProviderUsage>, String> {
        engine_usage::read_claude_usage_at(&self.home, shell_id)
    }
    fn read_grok(&self, ids: &[String], previous: &BTreeMap<String, engine_usage::GrokTabUsage>, force: bool) -> BTreeMap<String, engine_usage::GrokTabUsage> {
        let pids = riwork_engine::sessions::SessionManager::at(self.home.clone()).and_then(|sessions| sessions.pane_pids(ids));
        let targets = ids.iter().map(|id| engine_usage::GrokTarget {
            shell_id: id.clone(),
            pane_pid: match &pids {
                Ok(pids) => pids.get(id).copied().ok_or_else(|| "tmux did not report a process for this tab".to_owned()),
                Err(error) => Err(error.clone()),
            },
        }).collect::<Vec<_>>();
        engine_usage::read_grok_usages(&targets, previous, force)
    }
    fn cached_accounts(&self) -> Option<AccountsSnapshot> {
        // Woken on many home changes: parse again only when the cache moved.
        let current = stamp(&self.home.join(CACHE_NAME));
        let mut saved = self.saved.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((known, value)) = saved.as_ref()
            && *known == current
        {
            return value.clone();
        }
        let value = current.and_then(|_| read_saved_accounts(&self.home));
        *saved = Some((current, value.clone()));
        value
    }
    fn discover_accounts(&self) -> AccountsSnapshot {
        codex_accounts::discover_in(&self.home)
    }
    fn codex_settings(&self) -> Result<CodexSettings, String> {
        settings_of(&self.settings)
    }
    fn save_selected_account(&self, selected: Option<String>) -> Result<CodexSettings, String> {
        save_selected(&self.settings, selected)
    }
}

/// Headless and acceptance fixtures: canned Codex figures from
/// `<home>/usage-fixture.json`, the real file-only Claude and account caches,
/// and no process ever started (no codex, grok or Orca).
///
/// `{ "codex": { "<account id | system-default | absolute home>": <ProviderUsage
/// in engine form> | { "error": "…" } } }`; `updated_at_unix: 0` means "now".
#[cfg(any(debug_assertions, feature = "test-support"))]
pub struct FixtureUsageBackend {
    home: PathBuf,
    settings: SettingsStore,
}

#[cfg(any(debug_assertions, feature = "test-support"))]
impl FixtureUsageBackend {
    pub fn new(home: PathBuf) -> Result<Self, String> {
        Ok(Self {
            settings: SettingsStore::open(home.clone())?,
            home,
        })
    }
}

#[cfg(any(debug_assertions, feature = "test-support"))]
impl UsageBackend for FixtureUsageBackend {
    fn read_codex(&self, target: &CodexTarget) -> Result<ProviderUsage, String> {
        let unavailable = || FIXTURE_UNAVAILABLE.to_owned();
        let bytes = fs::read(self.home.join(FIXTURE_NAME)).map_err(|_| unavailable())?;
        let fixture: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
        let codex = fixture.get("codex").ok_or_else(unavailable)?;
        let id = target.account_id.as_deref().unwrap_or(SYSTEM_DEFAULT_ID);
        let entry = codex
            .get(id)
            .or_else(|| codex.get(target.home.to_string_lossy().as_ref()))
            .ok_or_else(unavailable)?;
        if let Some(error) = entry.get("error").and_then(serde_json::Value::as_str) {
            return Err(error.to_owned());
        }
        let mut usage: ProviderUsage =
            serde_json::from_value(entry.clone()).map_err(|_| unavailable())?;
        if usage.updated_at_unix == 0 {
            usage.updated_at_unix = SystemClock.now();
        }
        Ok(usage)
    }
    fn read_claude(&self, shell_id: &str) -> Result<Option<ProviderUsage>, String> {
        engine_usage::read_claude_usage_at(&self.home, shell_id)
    }
    fn cached_accounts(&self) -> Option<AccountsSnapshot> {
        read_saved_accounts(&self.home)
    }
    fn discover_accounts(&self) -> AccountsSnapshot {
        let saved = read_saved_accounts(&self.home);
        let from_cache = saved.is_some();
        let mut snapshot = saved.unwrap_or_else(|| AccountsSnapshot {
            accounts: system_account().into_iter().collect(),
            ..Default::default()
        });
        snapshot.error = Some(if from_cache {
            "Orca is not consulted in this fixture. Showing previously discovered accounts.".into()
        } else {
            "Orca is not consulted in this fixture.".into()
        });
        snapshot.from_cache = from_cache;
        snapshot
    }
    fn codex_settings(&self) -> Result<CodexSettings, String> {
        settings_of(&self.settings)
    }
    fn save_selected_account(&self, selected: Option<String>) -> Result<CodexSettings, String> {
        save_selected(&self.settings, selected)
    }
}

/// The process session inventory, drained without blocking on each pass.
pub trait ShellSource: Send + 'static {
    /// The newest inventory since the last call, if it changed.
    fn poll(&mut self) -> Option<Vec<ShellSessionDto>>;
}

impl<F: FnMut() -> Option<Vec<ShellSessionDto>> + Send + 'static> ShellSource for F {
    fn poll(&mut self) -> Option<Vec<ShellSessionDto>> {
        self()
    }
}

/// The native `Sessions` hub (no tmux call of its own).
pub struct TopicShells(pub TopicSubscription<SessionsSnapshot>);

impl ShellSource for TopicShells {
    fn poll(&mut self) -> Option<Vec<ShellSessionDto>> {
        let mut latest = None;
        while let Some(Ok(snapshot)) = self.0.recv().now_or_never() {
            latest = Some(snapshot.data.sessions);
        }
        latest
    }
}

/// Unix seconds. Tests replace it (§1.3 Clock).
pub trait UsageClock: Send + Sync + 'static {
    fn now(&self) -> u64;
}

pub struct SystemClock;

impl UsageClock for SystemClock {
    fn now(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

// ---- The process service ------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct CodexEntry {
    usage: Option<ProviderUsage>,
    error: Option<String>,
    pending: bool,
    last_attempt: u64,
    failures: u32,
}

#[derive(Clone, Debug, Default)]
struct ClaudeEntry {
    usage: Option<ProviderUsage>,
    error: Option<String>,
}

#[derive(Default)]
struct State {
    settings: CodexSettings,
    settings_error: Option<String>,
    system: Option<CodexAccount>,
    saved: Option<AccountsSnapshot>,
    discovered: Option<AccountsSnapshot>,
    refreshing: bool,
    discoveries: u64,
    shells: Vec<ShellSessionDto>,
    codex: BTreeMap<PathBuf, CodexEntry>,
    claude: BTreeMap<String, ClaudeEntry>,
    grok: BTreeMap<String, engine_usage::GrokTabUsage>,
    grok_pending: bool,
    force: bool,
    /// The last published usage body, with `now` cleared.
    published: Option<UsageDto>,
}

/// The account list as a launch would see it from memory.
struct AccountsView<'a> {
    listed: bool,
    accounts: Vec<&'a CodexAccount>,
    error: Option<&'a str>,
    from_cache: bool,
    source_active_id: Option<&'a str>,
}

impl State {
    fn view(&self) -> AccountsView<'_> {
        match self.discovered.as_ref().or(self.saved.as_ref()) {
            Some(snapshot) => AccountsView {
                listed: true,
                accounts: snapshot.accounts.iter().collect(),
                error: snapshot.error.as_deref(),
                from_cache: snapshot.from_cache,
                source_active_id: snapshot.source_active_id.as_deref(),
            },
            None => AccountsView {
                listed: false,
                accounts: self.system.iter().collect(),
                error: None,
                from_cache: false,
                source_active_id: None,
            },
        }
    }

    fn accounts_dto(&self) -> AccountsDto {
        let view = self.view();
        let selected = self
            .settings
            .selected
            .clone()
            .filter(|id| id != SYSTEM_DEFAULT_ID);
        AccountsDto {
            schema: SCHEMA,
            accounts: view
                .accounts
                .iter()
                .map(|account| {
                    let orca_active = view.source_active_id == Some(account.id.as_str());
                    let home = codex_accounts::display_home(&account.home);
                    CodexAccountDto {
                        id: account.id.clone(),
                        label: account.label.clone(),
                        email: account.email.clone(),
                        detail: account.unavailable_reason.clone().unwrap_or_else(|| {
                            if account.is_system_default {
                                format!("Current Codex profile · {home}")
                            } else if orca_active {
                                "Current Orca account".into()
                            } else {
                                "Saved in Orca".into()
                            }
                        }),
                        home,
                        home_path: account.home.to_string_lossy().into_owned(),
                        system: account.is_system_default,
                        available: account.available,
                        unavailable_reason: account.unavailable_reason.clone(),
                        saved: !account.is_system_default,
                        orca_active,
                    }
                })
                .collect(),
            selected_unavailable: selected.as_ref().is_some_and(|id| {
                view.listed
                    && !view
                        .accounts
                        .iter()
                        .any(|account| &account.id == id && account.available)
            }),
            selected_id: selected,
            balancing: self.settings.balancing,
            listed: view.listed,
            from_cache: view.from_cache,
            error: view
                .error
                .map(str::to_owned)
                .or_else(|| self.settings_error.clone()),
            refreshing: self.refreshing,
        }
    }

    /// Every available account in list order, deduplicated by home (before a
    /// list exists, the system default alone), then the homes live Codex
    /// shells run under (PANEL-183, SHELL-222).
    fn codex_targets(&self) -> Vec<CodexTarget> {
        let view = self.view();
        let mut homes = BTreeSet::new();
        let mut targets: Vec<CodexTarget> = view
            .accounts
            .iter()
            .filter(|account| account.available && homes.insert(account.home.clone()))
            .map(|account| CodexTarget {
                home: account.home.clone(),
                account_id: Some(account.id.clone()),
                label: if view.listed {
                    account.label.clone()
                } else {
                    SYSTEM_LABEL.into()
                },
            })
            .collect();
        for shell in self
            .shells
            .iter()
            .filter(|shell| shell.alive && shell.harness.as_deref() == Some("codex"))
        {
            let Some(home) = shell.codex_home.as_deref().map(PathBuf::from) else {
                continue;
            };
            if homes.insert(home.clone()) {
                targets.push(CodexTarget {
                    label: shell_label(shell, &home, &view),
                    account_id: shell.codex_account_id.clone(),
                    home,
                });
            }
        }
        targets
    }

    fn usage_dto(&self, now: u64) -> UsageDto {
        let codex: Vec<_> = self
            .codex_targets()
            .into_iter()
            .map(|target| {
                let entry = self.codex.get(&target.home).cloned().unwrap_or_default();
                CodexUsageDto {
                    home: target.home.to_string_lossy().into_owned(),
                    account_id: target.account_id,
                    label: target.label,
                    stale: entry.usage.as_ref().is_some_and(|usage| stale(usage, now)),
                    usage: entry.usage.as_ref().map(Into::into),
                    error: entry.error,
                    pending: entry.pending,
                    last_attempt: (entry.last_attempt != 0).then_some(entry.last_attempt),
                    failures: entry.failures,
                }
            })
            .collect();
        let claude = self
            .shells
            .iter()
            .filter(|shell| shell.harness.as_deref() == Some("claude"))
            .map(|shell| {
                let entry = self.claude.get(&shell.id).cloned().unwrap_or_default();
                ClaudeUsageDto {
                    shell_id: shell.id.clone(),
                    project_id: shell.project_id.clone(),
                    waiting: entry.usage.is_none(),
                    stale: entry.usage.as_ref().is_some_and(|usage| stale(usage, now)),
                    usage: entry
                        .usage
                        .as_ref()
                        .map(|usage| ProviderUsageDto::claude_at(usage, now)),
                    error: entry.error,
                }
            })
            .collect();
        UsageDto {
            schema: SCHEMA,
            now,
            refreshing: self.grok_pending || codex.iter().any(|entry: &CodexUsageDto| entry.pending),
            codex,
            claude,
            grok: self.shells.iter().filter(|shell| shell.alive && shell.harness.as_deref() == Some("grok")).map(|shell| {
                let entry = self.grok.get(&shell.id);
                GrokUsageDto {
                    shell_id: shell.id.clone(), project_id: shell.project_id.clone(),
                    label: format!("Grok · {}", shell.id.chars().take(8).collect::<String>()),
                    usage: entry.and_then(|entry| entry.usage.as_ref()).map(Into::into),
                    error: entry.and_then(|entry| entry.error.clone()),
                    stale: entry.is_some_and(|entry| entry.stale), pending: self.grok_pending,
                    fetched_at_unix: entry.map_or(0, |entry| entry.fetched_at_unix),
                }
            }).collect(),
        }
    }
}

/// `account_status.rs` `shell_selection`: list label, verified email, launch
/// label, else "System default" / "Session account".
fn shell_label(shell: &ShellSessionDto, home: &Path, view: &AccountsView<'_>) -> String {
    let listed = || view.accounts.iter().filter(|_| view.listed);
    listed()
        .find(|account| account.home == home)
        .map(|account| account.label.clone())
        .or_else(|| shell.codex_account_email.clone())
        .or_else(|| {
            listed()
                .find(|account| {
                    shell.codex_account_id.as_deref() == Some(account.id.as_str())
                        && account.home == home
                })
                .and_then(|account| account.email.clone())
        })
        .or_else(|| shell.codex_account_label.clone())
        .unwrap_or_else(|| {
            if shell.codex_account_id.is_none() {
                SYSTEM_LABEL.into()
            } else {
                "Session account".into()
            }
        })
}

pub fn stale(usage: &ProviderUsage, now: u64) -> bool {
    now.saturating_sub(usage.updated_at_unix) > STALE_AFTER_SECS
}

struct Inner {
    backend: Arc<dyn UsageBackend>,
    clock: Arc<dyn UsageClock>,
    shells: Mutex<Box<dyn ShellSource>>,
    state: Mutex<State>,
    /// Serializes passes with the writes whose results they must not undo.
    pass: Mutex<()>,
    discovery: Mutex<()>,
    usage_hub: Hub<UsageDto>,
    accounts_hub: Hub<AccountsDto>,
    grok_projects: Mutex<BTreeMap<String, usize>>,
    wake: SyncSender<()>,
    stop: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Inner {
    fn wake(&self) {
        let _ = self.wake.try_send(());
    }

    fn publish_accounts(&self, state: &State) {
        self.accounts_hub.publish(state.accounts_dto());
    }

    fn publish_usage(&self, state: &mut State, now: u64) {
        let body = state.usage_dto(now);
        let key = UsageDto {
            now: 0,
            ..body.clone()
        };
        if state.published.as_ref() != Some(&key) {
            state.published = Some(key);
            self.usage_hub.publish(body);
        }
    }

    /// Cheap file-only reads: settings, the saved list, the system home.
    fn reload(&self) {
        let settings = self.backend.codex_settings();
        let needs_saved = lock(&self.state).discovered.is_none();
        let saved = needs_saved.then(|| self.backend.cached_accounts());
        let system = self.backend.system_account().ok();
        let mut state = lock(&self.state);
        match settings {
            Ok(settings) => {
                state.settings = settings;
                state.settings_error = None;
            }
            Err(error) => state.settings_error = Some(error),
        }
        if let Some(saved) = saved {
            state.saved = saved;
        }
        state.system = system;
    }

    /// One evaluation. Returns the next unix deadline while subscribed.
    fn pass(self: &Arc<Self>) -> Option<u64> {
        let _pass = lock(&self.pass);
        let usage_subscribed = self.usage_hub.has_subscribers();
        let grok_projects = lock(&self.grok_projects).keys().cloned().collect::<BTreeSet<_>>();
        if !usage_subscribed && !self.accounts_hub.has_subscribers() {
            lock(&self.state).force = false;
            return None;
        }
        self.reload();
        self.publish_accounts(&lock(&self.state));
        if !usage_subscribed {
            lock(&self.state).force = false;
            return None;
        }
        if let Some(shells) = lock(&self.shells).poll() {
            lock(&self.state).shells = shells;
        }
        let claude_shells: Vec<String> = lock(&self.state)
            .shells
            .iter()
            .filter(|shell| shell.harness.as_deref() == Some("claude"))
            .map(|shell| shell.id.clone())
            .collect();
        let claude: BTreeMap<_, _> = claude_shells
            .into_iter()
            .map(|id| {
                let entry = match self.backend.read_claude(&id) {
                    Ok(usage) => ClaudeEntry { usage, error: None },
                    Err(error) => ClaudeEntry {
                        usage: None,
                        error: Some(error),
                    },
                };
                (id, entry)
            })
            .collect();
        let now = self.clock.now();
        let mut state = lock(&self.state);
        state.claude = claude;
        let force = std::mem::take(&mut state.force);
        let targets = state.codex_targets();
        let live_grok = state.shells.iter().filter(|shell| shell.alive && shell.harness.as_deref() == Some("grok"))
            .map(|shell| shell.id.clone()).collect::<BTreeSet<_>>();
        state.grok.retain(|id, _| live_grok.contains(id));
        let grok_ids = state.shells.iter().filter(|shell| shell.alive && shell.harness.as_deref() == Some("grok")
            && shell.project_id.as_ref().is_some_and(|id| grok_projects.contains(id))).map(|shell| shell.id.clone()).collect::<Vec<_>>();
        let grok_due = !state.grok_pending && !grok_ids.is_empty()
            && (force || grok_ids.iter().any(|id| state.grok.get(id).is_none_or(|entry| entry.is_due(now))));
        if grok_due { state.grok_pending = true; }
        let previous_grok = grok_due.then(|| state.grok.clone());
        let mut start = Vec::new();
        for target in &targets {
            let entry = state.codex.entry(target.home.clone()).or_default();
            let interval =
                engine_usage::codex_refresh_interval(entry.usage.is_some(), entry.failures);
            if entry.pending
                || (!force
                    && entry.last_attempt != 0
                    && now.saturating_sub(entry.last_attempt) < interval)
            {
                continue;
            }
            entry.pending = true;
            entry.last_attempt = now;
            start.push(target.clone());
        }
        self.publish_usage(&mut state, now);
        let mut next: Option<u64> = None;
        let mut soonest = |at: u64| next = Some(next.map_or(at, |next| next.min(at)));
        for target in &targets {
            let Some(entry) = state.codex.get(&target.home) else {
                continue;
            };
            if !entry.pending {
                soonest(
                    entry
                        .last_attempt
                        .saturating_add(engine_usage::codex_refresh_interval(
                            entry.usage.is_some(),
                            entry.failures,
                        )),
                );
            }
            if let Some(usage) = entry.usage.as_ref().filter(|usage| !stale(usage, now)) {
                soonest(usage.updated_at_unix + STALE_AFTER_SECS + 1);
            }
        }
        for usage in state
            .claude
            .values()
            .filter_map(|entry| entry.usage.as_ref())
        {
            if !stale(usage, now) {
                soonest(usage.updated_at_unix + STALE_AFTER_SECS + 1);
            }
            // A quiet or exited terminal may never send another status line.
            // Publish removal at each reset instead of retaining its old quota
            // until an unrelated service wake or a manual refresh.
            for reset in usage.windows.iter().filter_map(|window| window.resets_at) {
                if reset > now {
                    soonest(reset);
                }
            }
        }
        if !grok_ids.is_empty() { soonest(now + 5); }
        drop(state);
        if let Some(previous) = previous_grok { self.start_grok(grok_ids, previous, force); }
        for target in start {
            self.start_read(target);
        }
        next
    }

    fn start_grok(self: &Arc<Self>, ids: Vec<String>, previous: BTreeMap<String, engine_usage::GrokTabUsage>, force: bool) {
        let inner = self.clone();
        let failed_ids = ids.clone();
        let spawned = thread::Builder::new().name("riwork-usage-grok".into()).spawn(move || {
            let result = inner.backend.read_grok(&ids, &previous, force);
            { let mut state = lock(&inner.state); state.grok.extend(result); state.grok_pending = false; }
            inner.wake();
        });
        if let Err(error) = spawned {
            let mut state = lock(&self.state);
            state.grok_pending = false;
            for id in failed_ids { state.grok.insert(id, engine_usage::GrokTabUsage::unavailable(format!("Cannot start a usage read: {error}"))); }
            drop(state); self.wake();
        }
    }

    fn start_read(self: &Arc<Self>, target: CodexTarget) {
        let inner = self.clone();
        let home = target.home.clone();
        let spawned = thread::Builder::new()
            .name("riwork-usage-codex".into())
            .spawn(move || {
                let result = inner.backend.read_codex(&target);
                inner
                    .backend
                    .record_codex(&target.home, result.as_ref().ok());
                inner.finish_read(&target.home, result);
            });
        if let Err(error) = spawned {
            self.finish_read(&home, Err(format!("Cannot start a usage read: {error}")));
        }
    }

    fn finish_read(&self, home: &Path, result: Result<ProviderUsage, String>) {
        {
            let mut state = lock(&self.state);
            let entry = state.codex.entry(home.to_owned()).or_default();
            entry.pending = false;
            match result {
                Ok(usage) => {
                    entry.usage = Some(usage);
                    entry.error = None;
                    entry.failures = 0;
                }
                Err(error) => {
                    entry.error = Some(error);
                    entry.failures = entry.failures.saturating_add(1);
                }
            }
        }
        self.wake();
    }
}

struct GrokProjectLease { inner: Arc<Inner>, project: String }
impl Drop for GrokProjectLease {
    fn drop(&mut self) {
        let mut projects = lock(&self.inner.grok_projects);
        if let Some(count) = projects.get_mut(&self.project) {
            *count = count.saturating_sub(1);
            if *count == 0 { projects.remove(&self.project); }
        }
        drop(projects); self.inner.wake();
    }
}

/// The process-wide account and usage service. Dropping it stops its thread.
pub struct UsageService {
    inner: Arc<Inner>,
    worker: Option<JoinHandle<()>>,
}

impl UsageService {
    pub fn start(
        backend: Arc<dyn UsageBackend>,
        clock: Arc<dyn UsageClock>,
        shells: Box<dyn ShellSource>,
    ) -> Result<Arc<Self>, String> {
        let (wake, receive) = mpsc::sync_channel(1);
        let inner = Arc::new(Inner {
            backend,
            clock,
            shells: Mutex::new(shells),
            state: Mutex::new(State::default()),
            pass: Mutex::new(()),
            discovery: Mutex::new(()),
            usage_hub: Hub::pending(),
            accounts_hub: Hub::pending(),
            grok_projects: Mutex::new(BTreeMap::new()),
            wake,
            stop: AtomicBool::new(false),
        });
        let worker_inner = inner.clone();
        let worker = thread::Builder::new()
            .name("riwork-native-usage".into())
            .spawn(move || run(worker_inner, receive))
            .map_err(service::io_error)?;
        Ok(Arc::new(Self {
            inner,
            worker: Some(worker),
        }))
    }

    /// A sender that re-evaluates the service; wired to the home observers.
    pub fn waker(&self) -> SyncSender<()> {
        self.inner.wake.clone()
    }

    pub fn wake(&self) {
        self.inner.wake();
    }

    pub fn subscribe_usage(&self) -> TopicSubscription<UsageDto> {
        let subscription = self.inner.usage_hub.subscribe();
        self.inner.wake();
        subscription
    }

    fn project_lease(&self, project: String) -> GrokProjectLease {
        *lock(&self.inner.grok_projects).entry(project.clone()).or_default() += 1;
        self.inner.wake();
        GrokProjectLease { inner: self.inner.clone(), project }
    }

    pub fn subscribe_accounts(&self) -> TopicSubscription<AccountsDto> {
        let subscription = self.inner.accounts_hub.subscribe();
        self.inner.wake();
        subscription
    }

    /// `force` ignores the backoff (never a read in flight). Without a
    /// subscriber nothing is read.
    pub fn refresh_usage(&self, force: bool) {
        if force {
            lock(&self.inner.state).force = true;
        }
        self.inner.wake();
    }

    /// Runs discovery once; a caller arriving during one shares its result.
    pub fn refresh_accounts(&self) -> AccountsDto {
        let inner = &self.inner;
        let seen = lock(&inner.state).discoveries;
        let _discovery = lock(&inner.discovery);
        if lock(&inner.state).discoveries != seen {
            return lock(&inner.state).accounts_dto();
        }
        {
            let _pass = lock(&inner.pass);
            let mut state = lock(&inner.state);
            state.refreshing = true;
            inner.publish_accounts(&state);
        }
        let snapshot = inner.backend.discover_accounts();
        inner.reload();
        let result = {
            let _pass = lock(&inner.pass);
            let mut state = lock(&inner.state);
            state.discovered = Some(snapshot);
            state.discoveries += 1;
            state.refreshing = false;
            inner.publish_accounts(&state);
            state.accounts_dto()
        };
        inner.wake();
        result
    }

    /// PANEL-147: null or the system default's id saves the system default;
    /// a saved id must be an available listed account.
    pub fn select_default(&self, id: Option<String>) -> Result<AccountsDto, String> {
        let inner = &self.inner;
        // The list a choice is checked against, even with no subscriber.
        inner.reload();
        let _pass = lock(&inner.pass);
        let selected = match id {
            None => None,
            Some(id) => {
                let state = lock(&inner.state);
                let view = state.view();
                let account = view
                    .accounts
                    .iter()
                    .find(|account| account.id == id && account.available)
                    .ok_or_else(|| format!("refused: {UNAVAILABLE_ACCOUNT}"))?;
                (!account.is_system_default).then(|| account.id.clone())
            }
        };
        let settings = inner.backend.save_selected_account(selected)?;
        let mut state = lock(&inner.state);
        state.settings = settings;
        state.settings_error = None;
        inner.publish_accounts(&state);
        Ok(state.accounts_dto())
    }

    /// The current accounts payload, built from memory.
    pub fn accounts(&self) -> AccountsDto {
        lock(&self.inner.state).accounts_dto()
    }
}

impl Drop for UsageService {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Release);
        self.inner.wake();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run(inner: Arc<Inner>, wake: Receiver<()>) {
    while !inner.stop.load(Ordering::Acquire) {
        let deadline = inner.pass();
        if inner.stop.load(Ordering::Acquire) {
            break;
        }
        match deadline {
            Some(at) => {
                let wait = at.saturating_sub(inner.clock.now()).max(1);
                let _ = wake.recv_timeout(Duration::from_secs(wait));
            }
            // No subscriber, or nothing timed: only a wake starts work again.
            None => {
                if wake.recv().is_err() {
                    break;
                }
            }
        }
    }
}

// ---- The process instance -----------------------------------------------------------------

static SERVICE: OnceLock<Arc<UsageService>> = OnceLock::new();
static START: Mutex<()> = Mutex::new(());

/// Host and test seam: install before the first `Usage`/`Accounts` call.
pub fn install_usage_service(service: Arc<UsageService>) -> Result<(), String> {
    let _start = lock(&START);
    SERVICE
        .set(service)
        .map_err(|_| "conflict: usage service is already initialized".into())
}

/// Debug and test-support builds never start codex, grok or Orca for a fixture
/// home: `RIWORK_NSH_FAKE_SESSIONS` is set, or the home is under `/tmp` (the
/// headless `nsh test` runner builds without `test-support`).
#[cfg(any(debug_assertions, feature = "test-support"))]
fn fixture_home(home: &Path) -> bool {
    let home = home.canonicalize().unwrap_or_else(|_| home.to_owned());
    std::env::var_os("RIWORK_NSH_FAKE_SESSIONS").is_some_and(|path| !path.is_empty())
        || home.starts_with("/tmp")
        || home.starts_with("/private/tmp")
}

fn process_backend(home: &Path) -> Result<Arc<dyn UsageBackend>, String> {
    #[cfg(any(debug_assertions, feature = "test-support"))]
    if fixture_home(home) {
        return Ok(Arc::new(FixtureUsageBackend::new(home.to_owned())?));
    }
    Ok(Arc::new(CoreUsageBackend::new(home.to_owned())?))
}

async fn usage_service() -> Result<Arc<UsageService>, String> {
    if let Some(service) = SERVICE.get() {
        return Ok(service.clone());
    }
    let process = service::initialize().await?;
    let sessions = process.subscribe_sessions().await?;
    blocking(move || {
        let _start = lock(&START);
        if let Some(service) = SERVICE.get() {
            return Ok(service.clone());
        }
        let usage = UsageService::start(
            process_backend(process.home())?,
            Arc::new(SystemClock),
            Box::new(TopicShells(sessions)),
        )?;
        process.observe_changes(usage.waker());
        let _ = SERVICE.set(usage.clone());
        Ok(usage)
    })
    .await
}

// ---- Modules ------------------------------------------------------------------------------

/// Codex accounts (PANEL-143..149, PANEL-186..190).
#[module]
pub struct Accounts;

#[module]
impl Accounts {
    /// Cache-only form read. No account discovery or credential access.
    pub async fn list() -> Result<AccountsDto, String> {
        let service = usage_service().await?;
        blocking(move || {
            service.inner.reload();
            Ok(service.accounts())
        })
        .await
    }
    /// Cache-only: never starts Orca. Never periodic.
    pub fn subscribe(
        listener: Callback<Snapshot<AccountsDto>>,
        cx: CallCx<'_>,
    ) -> Result<crate::Subscription, String> {
        Ok(crate::Subscription::start(cx, async move {
            if let Ok(service) = usage_service().await
                && let Ok(subscription) = blocking(move || Ok(service.subscribe_accounts())).await
            {
                crate::deliver(subscription, listener).await;
            }
        }))
    }

    /// Runs Orca's account discovery once (single-flight), then republishes.
    pub async fn refresh() -> Result<AccountsDto, String> {
        let service = usage_service().await?;
        blocking(move || Ok(service.refresh_accounts())).await
    }

    /// The app's default account; null is the system default.
    pub async fn select_default(id: Option<String>) -> Result<AccountsDto, String> {
        let service = usage_service().await?;
        blocking(move || service.select_default(id)).await
    }
}

/// Codex and Claude usage (PANEL-064..066, PANEL-180..185, SHELL-221..224).
#[module]
pub struct Usage;

#[module]
impl Usage {
    /// Codex reads run only while subscribed. A visible Usage panel passes its
    /// project to include that project's live Grok session figures.
    pub fn subscribe(
        listener: Callback<Snapshot<UsageDto>>,
        project_id: Option<String>,
        cx: CallCx<'_>,
    ) -> Result<crate::Subscription, String> {
        Ok(crate::Subscription::start(cx, async move {
            if let Ok(service) = usage_service().await {
                let source = service.clone();
                let registration = blocking(move || {
                    let lease = project_id.filter(|id| uuid::Uuid::parse_str(id).is_ok()).map(|project| source.project_lease(project));
                    Ok((source.subscribe_usage(), lease))
                }).await;
                if let Ok((subscription, _lease)) = registration { crate::deliver(subscription, listener).await; }
            }
        }))
    }

    /// Re-evaluates now; `force` also ignores the backoff (PANEL-065).
    pub async fn refresh(force: bool) -> Result<(), String> {
        let service = usage_service().await?;
        blocking(move || {
            service.refresh_usage(force);
            Ok(())
        })
        .await
    }
}

use nsh_core::{
    JsonValue,
    element::{JsHost, JsIn, JsOwned, PropError, TsTypes},
};

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

snapshot_js_value!(UsageDto, "UsageEvent");
snapshot_js_value!(AccountsDto, "AccountsEvent");

#[cfg(test)]
mod tests {
    use super::*;
    use riwork_engine::usage::UsageWindow;

    fn account(id: &str, label: &str, home: &str, available: bool) -> CodexAccount {
        CodexAccount {
            id: id.into(),
            label: label.into(),
            email: label.contains('@').then(|| label.into()),
            home: home.into(),
            available,
            unavailable_reason: (!available)
                .then(|| "This saved account's home is missing.".to_owned()),
            is_system_default: id == SYSTEM_DEFAULT_ID,
        }
    }

    fn list() -> AccountsSnapshot {
        AccountsSnapshot {
            accounts: vec![
                account(SYSTEM_DEFAULT_ID, SYSTEM_LABEL, "/fixture/system", true),
                account("account-a", "a@example.test", "/fixture/a", true),
                account("account-b", "b@example.test", "/fixture/b", true),
                account("account-dup", "dup@example.test", "/fixture/a", true),
                account("account-c", "c@example.test", "/fixture/c", false),
            ],
            error: None,
            source_active_id: Some("account-b".into()),
            from_cache: false,
        }
    }

    fn shell(json: serde_json::Value) -> ShellSessionDto {
        let mut base = serde_json::json!({
            "userOpened": true, "parentId": null, "projectId": "p", "worktreeId": null,
            "kind": "project", "cwd": "/w", "command": null, "editorPath": null,
            "harness": null, "unrestricted": false, "codexAccountId": null,
            "codexAccountLabel": null, "codexAccountEmail": null, "codexHome": null,
            "orchestratorSkillLoaded": false, "orchestratorSkillVersion": null,
            "orchestratorDelegationVersion": null, "orchestratorProjectRoot": null,
            "createdAtUnix": 1, "alive": true
        });
        for (key, value) in json.as_object().unwrap() {
            base[key] = value.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    fn quota(updated: u64) -> ProviderUsage {
        ProviderUsage {
            provider: "codex".into(),
            windows: vec![UsageWindow {
                label: "5h".into(),
                used_percent: 20.0,
                resets_at: None,
                window_minutes: Some(300),
            }],
            updated_at_unix: updated,
            account_label: None,
            context_used_percent: None,
            session_cost_usd: None,
            session: None,
            session_error: None,
        }
    }

    #[test]
    fn usage_accounts_list_saved_accounts_with_panel_146_detail_lines() {
        let state = State {
            saved: Some(list()),
            settings: CodexSettings {
                selected: Some("account-c".into()),
                balancing: true,
            },
            ..Default::default()
        };
        let dto = state.accounts_dto();
        assert!(dto.listed && dto.balancing && !dto.from_cache && dto.selected_unavailable);
        assert_eq!(
            dto.accounts
                .iter()
                .map(|account| (account.id.as_str(), account.detail.as_str(), account.saved))
                .collect::<Vec<_>>(),
            [
                (
                    SYSTEM_DEFAULT_ID,
                    "Current Codex profile · /fixture/system",
                    false
                ),
                ("account-a", "Saved in Orca", true),
                ("account-b", "Current Orca account", true),
                ("account-dup", "Saved in Orca", true),
                ("account-c", "This saved account's home is missing.", true),
            ]
        );
        assert_eq!(dto.accounts[0].home_path, "/fixture/system");
        // The system default's id is the system default: no saved selection.
        let state = State {
            settings: CodexSettings {
                selected: Some(SYSTEM_DEFAULT_ID.into()),
                balancing: false,
            },
            system: Some(account(SYSTEM_DEFAULT_ID, SYSTEM_LABEL, "/x", true)),
            ..Default::default()
        };
        let dto = state.accounts_dto();
        assert_eq!(dto.selected_id, None);
        assert!(!dto.listed && dto.accounts.len() == 1 && !dto.selected_unavailable);
    }

    #[test]
    fn usage_targets_follow_the_list_dedupe_homes_and_add_live_codex_shell_homes() {
        let mut state = State {
            saved: Some(list()),
            shells: vec![
                shell(
                    serde_json::json!({"id": "s1", "harness": "codex", "codexHome": "/fixture/b"}),
                ),
                shell(
                    serde_json::json!({"id": "s2", "harness": "codex", "codexHome": "/gone",
                    "codexAccountId": "old", "codexAccountLabel": "old@example.test · Old"}),
                ),
                shell(
                    serde_json::json!({"id": "s3", "harness": "codex", "codexHome": "/dead", "alive": false}),
                ),
                shell(
                    serde_json::json!({"id": "s4", "harness": "codex", "codexHome": "/other", "codexAccountId": "x"}),
                ),
            ],
            ..Default::default()
        };
        let targets = state.codex_targets();
        assert_eq!(
            targets
                .iter()
                .map(|target| (target.home.to_str().unwrap(), target.label.as_str()))
                .collect::<Vec<_>>(),
            [
                ("/fixture/system", SYSTEM_LABEL),
                ("/fixture/a", "a@example.test"),
                ("/fixture/b", "b@example.test"),
                ("/gone", "old@example.test · Old"),
                ("/other", "Session account"),
            ]
        );
        // Before a list exists only the system default (and shell homes) are read.
        state.saved = None;
        state.shells.clear();
        state.system = Some(account(SYSTEM_DEFAULT_ID, SYSTEM_LABEL, "/sys", true));
        assert_eq!(state.codex_targets().len(), 1);
    }

    #[test]
    fn usage_entries_mark_old_figures_stale_and_claude_shells_waiting() {
        let mut state = State {
            system: Some(account(SYSTEM_DEFAULT_ID, SYSTEM_LABEL, "/sys", true)),
            shells: vec![
                shell(serde_json::json!({"id": "c1", "harness": "claude"})),
                shell(serde_json::json!({"id": "c2", "harness": "claude"})),
            ],
            ..Default::default()
        };
        state.codex.insert(
            "/sys".into(),
            CodexEntry {
                usage: Some(quota(1_000)),
                last_attempt: 1_000,
                ..Default::default()
            },
        );
        state.claude.insert(
            "c1".into(),
            ClaudeEntry {
                usage: Some(quota(1_000)),
                error: None,
            },
        );
        let fresh = state.usage_dto(1_000 + STALE_AFTER_SECS);
        assert!(!fresh.codex[0].stale && !fresh.claude[0].stale);
        let old = state.usage_dto(1_001 + STALE_AFTER_SECS);
        assert!(old.codex[0].stale && old.claude[0].stale);
        assert_eq!(old.codex[0].last_attempt, Some(1_000));
        assert!(!old.claude[0].waiting && old.claude[1].waiting);
        assert!(old.grok.is_empty() && !old.refreshing);
    }
}
