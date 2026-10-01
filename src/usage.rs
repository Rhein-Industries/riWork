//! Usage supplied by the official harnesses, without reading credentials or
//! replaying terminal input. Codex reads run on a worker; Claude pushes its
//! documented status-line payload into a small per-shell cache; Grok's own
//! `grok usage` reports one session's tokens and cost on a worker. Grok's
//! account allowance has no official interface and is never read.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    fs::OpenOptions,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const CODEX_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_SERVER_OUTPUT: u64 = 2 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024;
/// `grok usage` only reads a local session file, so it answers well inside this.
const GROK_TIMEOUT: Duration = Duration::from_secs(5);
/// The report lists every turn. A session of thousands of turns still fits.
const MAX_GROK_OUTPUT: u64 = 4 * 1024 * 1024;
const MAX_ACTIVE_SESSIONS_BYTES: u64 = 1024 * 1024;
const GROK_TICKS_PER_USD: f64 = 1e10;
/// A process may start a little after the wall-clock time Grok recorded for the
/// session (clock steps, coarse timestamps). Starting later than that means the
/// pid belongs to something else.
const GROK_START_SLACK_SECS: f64 = 60.0;
/// A window asks again this often while Grok usage is on screen.
pub const GROK_USAGE_REFRESH: Duration = Duration::from_secs(30);
/// Slightly under the refresh period, so a window's next tick is not answered
/// from the read its previous tick took.
const GROK_CACHE_TTL: Duration = Duration::from_secs(25);
/// A failed read of a session that has had good figures (Grok mid-write, a slow
/// disk) is tried again sooner.
const GROK_RETRY_AFTER_FAILURE: Duration = Duration::from_secs(10);
const GROK_PARALLEL_READS: usize = 6;
const GROK_CACHE_LIMIT: usize = 256;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub provider: String,
    pub windows: Vec<UsageWindow>,
    pub updated_at_unix: u64,
    pub account_label: Option<String>,
    #[serde(default)]
    pub context_used_percent: Option<f64>,
    #[serde(default)]
    pub session_cost_usd: Option<f64>,
    /// Grok's report for one session. Absent for the other providers, so their
    /// output does not change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionUsage>,
    /// Why `session` is missing or out of date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cached_read: u64,
    pub cache_creation: u64,
    pub reasoning: u64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub model: String,
    pub model_calls: u64,
    pub tokens: TokenCounts,
    pub cost_usd: Option<f64>,
}

/// What `grok usage <session>` reports for one local session, which includes
/// history inherited by a resume or fork.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionUsage {
    pub session_id: String,
    /// Grok's own `updatedAt`: the session's last recorded activity.
    pub updated_at: Option<String>,
    pub updated_at_unix: Option<u64>,
    pub primary_model: Option<String>,
    pub turns: u64,
    pub model_calls: u64,
    pub tokens: TokenCounts,
    pub cost_usd: Option<f64>,
    pub models: Vec<ModelUsage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub label: String,
    pub used_percent: f64,
    pub resets_at: Option<u64>,
    pub window_minutes: Option<u64>,
}

/// Seconds before an account's quota is read again. An account that keeps
/// failing (signed out, offline, broken install) would otherwise start a
/// `codex app-server` every minute for as long as RiWork is open, so each
/// consecutive failure doubles the wait up to a cap. A forced refresh ignores
/// this, and any success starts over.
pub fn codex_refresh_interval(has_snapshot: bool, failures: u32) -> u64 {
    // The caps stay short enough that signing in again is noticed soon.
    let (base, cap) = if has_snapshot {
        (15 * 60, 30 * 60)
    } else {
        (60, 10 * 60)
    };
    (base << failures.min(6)).min(cap)
}

/// Read the account and its quota through Codex's app-server under this exact
/// home. The child override does not change this process's environment.
/// Blocking and bounded: call from a worker, never GPUI's render thread.
/// No thread, turn, model, login, or shell input request is sent to Codex.
pub fn read_codex_usage_at(home: &Path) -> Result<ProviderUsage, String> {
    let executable = find_codex().ok_or("Codex is not installed or is not on PATH")?;
    read_codex_usage_with(&executable, Some(home), CODEX_TIMEOUT)
}

fn read_codex_usage_with(
    executable: &Path,
    home: Option<&Path>,
    timeout: Duration,
) -> Result<ProviderUsage, String> {
    let mut server = CodexServer::start(executable, home)?;
    let deadline = Instant::now() + timeout;
    server.send(json!({
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": { "name": "riwork_usage", "title": "RiWork usage", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "experimentalApi": false }
        }
    }))?;
    server.response(1, deadline)?;
    server.send(json!({ "method": "initialized" }))?;
    server
        .send(json!({ "id": 2, "method": "account/read", "params": { "refreshToken": false } }))?;
    server.send(json!({ "id": 3, "method": "account/rateLimits/read", "params": null }))?;

    let mut account = None;
    let mut account_received = false;
    let mut rates = None;
    let mut finish_by = deadline;
    loop {
        let message = match server.next(finish_by) {
            Ok(message) => message,
            Err(_) if rates.is_some() => break,
            Err(error) => return Err(error),
        };
        match message.get("id").and_then(Value::as_u64) {
            Some(2) => {
                account_received = true;
                account = message.get("result").cloned();
            }
            Some(3) => {
                let response = response_result(&message)?;
                rates = Some(response);
                // Account labels are optional. Do not delay a valid quota read
                // for more than a second if its account reply is still pending.
                finish_by = deadline.min(Instant::now() + Duration::from_secs(1));
            }
            _ => {}
        }
        if account_received && rates.is_some() {
            break;
        }
    }

    let rates = rates.ok_or("Codex did not return subscription usage")?;
    let label = account.as_ref().and_then(codex_account_label);
    parse_codex_usage(&rates, label, unix_now())
}

/// Store only quota windows, context percentage, and estimated session cost.
/// Claude's larger payload (transcript paths, workspace data, and arbitrary
/// future fields) is deliberately never written to disk.
pub fn record_claude_status(shell_id: &str, value: &Value) -> Result<ProviderUsage, String> {
    let home = usage_home()?;
    record_claude_status_at(&home, shell_id, value)
}

pub fn read_claude_usage(shell_id: &str) -> Result<Option<ProviderUsage>, String> {
    read_claude_usage_at(&usage_home()?, shell_id)
}

pub fn read_claude_usage_at(home: &Path, shell_id: &str) -> Result<Option<ProviderUsage>, String> {
    let path = cache_path(home, shell_id)?;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read Claude usage cache".into()),
    };
    if file
        .metadata()
        .map_err(|_| "Cannot inspect Claude usage cache")?
        .len()
        > MAX_CACHE_BYTES
    {
        return Err("Claude usage cache is too large".into());
    }
    let usage: ProviderUsage = serde_json::from_reader(file.take(MAX_CACHE_BYTES))
        .map_err(|_| "Claude usage cache is invalid")?;
    if usage.provider != "claude"
        || usage.windows.len() > 16
        || usage
            .windows
            .iter()
            .any(|window| !valid_number(window.used_percent))
        || usage
            .context_used_percent
            .is_some_and(|value| !valid_number(value))
        || usage
            .session_cost_usd
            .is_some_and(|value| !valid_number(value))
    {
        return Err("Claude usage cache is invalid".into());
    }
    Ok(Some(usage))
}

/// A short status line for the interactive CLI, alongside the GPUI readout.
pub fn claude_status_text(value: &Value) -> String {
    let model = value
        .pointer("/model/display_name")
        .and_then(Value::as_str)
        .map(clean_label)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Claude".into());
    let mut parts = vec![model];
    if let Some(percent) = number_at(value, "/context_window/used_percentage") {
        parts.push(format!("CTX {percent:.0}%"));
    }
    if let Some(cost) = number_at(value, "/cost/total_cost_usd") {
        parts.push(format!("EST ${cost:.2}"));
    }
    for (key, label) in [("five_hour", "5h"), ("seven_day", "7d")] {
        if let Some(percent) = number_at(value, &format!("/rate_limits/{key}/used_percentage")) {
            parts.push(format!("{label} {percent:.0}% used"));
        }
    }
    parts.join(" · ")
}

fn record_claude_status_at(
    home: &Path,
    shell_id: &str,
    value: &Value,
) -> Result<ProviderUsage, String> {
    let path = cache_path(home, shell_id)?;
    let usage = parse_claude_usage(value, unix_now())?;
    let directory = path.parent().ok_or("Invalid usage cache path")?;
    fs::create_dir_all(directory).map_err(|_| "Cannot create Claude usage cache directory")?;
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| "Cannot write Claude usage cache")?;
        serde_json::to_writer(&mut file, &usage).map_err(|_| "Cannot encode Claude usage cache")?;
        file.write_all(b"\n")
            .map_err(|_| "Cannot write Claude usage cache")?;
        file.sync_all()
            .map_err(|_| "Cannot save Claude usage cache")?;
        fs::rename(&temporary, &path).map_err(|_| "Cannot replace Claude usage cache")?;
        Ok(usage)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn parse_claude_usage(value: &Value, now: u64) -> Result<ProviderUsage, String> {
    if !value.is_object() {
        return Err("Claude status must be a JSON object".into());
    }
    let mut windows = Vec::new();
    for (key, label, minutes) in [
        ("five_hour", "5h", Some(300)),
        ("seven_day", "7d", Some(10080)),
        ("spend_limit", "Spend limit", None),
    ] {
        let Some(window) = value.pointer(&format!("/rate_limits/{key}")) else {
            continue;
        };
        let Some(percent) = window.get("used_percentage").and_then(number) else {
            continue;
        };
        windows.push(UsageWindow {
            label: label.into(),
            used_percent: percent,
            resets_at: positive_u64(window.get("resets_at")),
            window_minutes: minutes,
        });
    }
    Ok(ProviderUsage {
        provider: "claude".into(),
        windows,
        updated_at_unix: now,
        // The supported status-line schema contains no account identity.
        account_label: None,
        context_used_percent: number_at(value, "/context_window/used_percentage"),
        session_cost_usd: number_at(value, "/cost/total_cost_usd"),
        session: None,
        session_error: None,
    })
}

fn parse_codex_usage(
    value: &Value,
    account_label: Option<String>,
    now: u64,
) -> Result<ProviderUsage, String> {
    if !value.is_object() {
        return Err("Codex returned an invalid usage response".into());
    }
    let mut windows = Vec::new();
    if let Some(buckets) = value.get("rateLimitsByLimitId").and_then(Value::as_object) {
        for (id, bucket) in buckets {
            append_codex_windows(bucket, Some(id), &mut windows);
        }
    }
    // The legacy field mirrors a bucket in the multi-bucket response. It is
    // only a fallback, so the same limits are never shown twice.
    if windows.is_empty() {
        if let Some(bucket) = value.get("rateLimits") {
            append_codex_windows(bucket, None, &mut windows);
        }
    }
    if windows.is_empty() {
        return Err(
            "Codex has no subscription usage windows available; check its account sign-in".into(),
        );
    }
    let mut seen = BTreeSet::new();
    windows.retain(|window| {
        seen.insert((
            window.label.clone(),
            window.used_percent.to_bits(),
            window.resets_at,
            window.window_minutes,
        ))
    });
    Ok(ProviderUsage {
        provider: "codex".into(),
        windows,
        updated_at_unix: now,
        account_label,
        context_used_percent: None,
        session_cost_usd: None,
        session: None,
        session_error: None,
    })
}

fn append_codex_windows(bucket: &Value, id: Option<&str>, windows: &mut Vec<UsageWindow>) {
    let name = bucket
        .get("limitName")
        .and_then(Value::as_str)
        .or_else(|| bucket.get("limitId").and_then(Value::as_str))
        .or(id)
        .map(clean_label)
        .filter(|name| !name.is_empty());
    for (key, fallback) in [("primary", "Primary"), ("secondary", "Secondary")] {
        let Some(window) = bucket.get(key) else {
            continue;
        };
        let Some(percent) = window.get("usedPercent").and_then(number) else {
            continue;
        };
        let minutes = positive_u64(window.get("windowDurationMins"));
        let duration = minutes
            .map(duration_label)
            .unwrap_or_else(|| fallback.into());
        let label = match name.as_deref() {
            Some("codex") | None => duration,
            Some(name) => format!("{name} {duration}"),
        };
        windows.push(UsageWindow {
            label,
            used_percent: percent,
            resets_at: positive_u64(window.get("resetsAt")),
            window_minutes: minutes,
        });
    }
}

fn duration_label(minutes: u64) -> String {
    if minutes.is_multiple_of(1440) {
        format!("{}d", minutes / 1440)
    } else if minutes.is_multiple_of(60) {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

fn codex_account_label(value: &Value) -> Option<String> {
    let account = value.get("account")?;
    match account.get("type")?.as_str()? {
        "chatgpt" => {
            let email = account
                .get("email")
                .and_then(Value::as_str)
                .map(clean_label)
                .filter(|value| !value.is_empty());
            let plan = account
                .get("planType")
                .and_then(Value::as_str)
                .map(clean_label)
                .filter(|value| !value.is_empty());
            match (email, plan) {
                (Some(email), Some(plan)) => Some(format!("{email} · {plan}")),
                (email, plan) => email.or(plan),
            }
        }
        "apiKey" => Some("API key".into()),
        "amazonBedrock" => Some("Amazon Bedrock".into()),
        _ => None,
    }
}

/// One Grok tab whose session usage is wanted.
pub struct GrokTarget {
    pub shell_id: String,
    /// The process the tab's pane runs, or why tmux could not say.
    pub pane_pid: Result<u32, String>,
}

/// What is known about one Grok tab's session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GrokTabUsage {
    pub usage: Option<SessionUsage>,
    /// `usage` is an earlier read: the latest attempt failed.
    pub stale: bool,
    pub error: Option<String>,
    /// When `usage` was read; zero without one.
    pub fetched_at_unix: u64,
    /// When this was last worked out, successfully or not.
    pub checked_at_unix: u64,
    /// The pane process this was worked out for.
    pane_pid: Option<u32>,
}

impl GrokTabUsage {
    /// A tab that has nothing to report, and why.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            error: Some(reason.into()),
            checked_at_unix: unix_now(),
            ..Self::default()
        }
    }

    /// Whether a window showing this should read it again: on the refresh
    /// period, or sooner while there are no figures yet (a tab that has just
    /// started is not in Grok's list for its first moments).
    pub fn is_due(&self, now: u64) -> bool {
        let wait = if self.usage.is_some() {
            GROK_USAGE_REFRESH
        } else {
            GROK_RETRY_AFTER_FAILURE / 2
        };
        now.saturating_sub(self.checked_at_unix) >= wait.as_secs()
    }
}

/// Session usage for each target, keyed by shell ID. Blocking and bounded (each
/// `grok usage` has a timeout): call from a worker, never GPUI's render thread.
/// Reads are cached process-wide for a short time per Grok session, so windows
/// and ticks that ask again share one; `force` reads again regardless.
/// `previous` supplies the last good figures for a tab whose latest read failed.
pub fn read_grok_usages(
    targets: &[GrokTarget],
    previous: &BTreeMap<String, GrokTabUsage>,
    force: bool,
) -> BTreeMap<String, GrokTabUsage> {
    static CACHE: GrokUsageCache = GrokUsageCache::new(GROK_CACHE_TTL);
    let reader = GrokReader {
        home: grok_home(),
        executable: crate::sessions::find_grok_program()
            .ok_or_else(|| "Grok is not installed or is not on PATH".to_owned()),
        timeout: GROK_TIMEOUT,
        cache: &CACHE,
        processes: &SystemProcesses,
    };
    read_grok_usages_with(&reader, targets, previous, force)
}

struct GrokReader<'a> {
    /// Where Grok keeps `active_sessions.json`.
    home: Result<PathBuf, String>,
    /// The official CLI, never RiWork's launcher.
    executable: Result<PathBuf, String>,
    timeout: Duration,
    cache: &'a GrokUsageCache,
    processes: &'a dyn ProcessProbe,
}

fn read_grok_usages_with(
    reader: &GrokReader<'_>,
    targets: &[GrokTarget],
    previous: &BTreeMap<String, GrokTabUsage>,
    force: bool,
) -> BTreeMap<String, GrokTabUsage> {
    let now = unix_now();
    let mut results = BTreeMap::new();
    if targets.is_empty() {
        return results;
    }
    let entries = reader
        .home
        .clone()
        .and_then(|home| read_active_grok_sessions(&home));
    // (shell, pane process, Grok session)
    let mut jobs = Vec::new();
    for target in targets {
        let before = previous.get(&target.shell_id);
        let pid = match &target.pane_pid {
            Ok(pid) => *pid,
            Err(error) => {
                results.insert(
                    target.shell_id.clone(),
                    keep_stale(before, None, error, now),
                );
                continue;
            }
        };
        let entries = match &entries {
            Ok(entries) => entries,
            Err(error) => {
                results.insert(
                    target.shell_id.clone(),
                    keep_stale(before, Some(pid), error, now),
                );
                continue;
            }
        };
        match resolve_grok_session(entries, pid, reader.processes) {
            Ok(session) => jobs.push((target.shell_id.clone(), pid, session)),
            Err(reason) => {
                results.insert(
                    target.shell_id.clone(),
                    GrokTabUsage {
                        error: Some(reason),
                        checked_at_unix: now,
                        pane_pid: Some(pid),
                        ..GrokTabUsage::default()
                    },
                );
            }
        }
    }

    let executable = match &reader.executable {
        Ok(executable) => executable.as_path(),
        Err(error) => {
            for (shell, pid, _) in jobs {
                results.insert(
                    shell,
                    GrokTabUsage {
                        error: Some(error.clone()),
                        checked_at_unix: now,
                        pane_pid: Some(pid),
                        ..GrokTabUsage::default()
                    },
                );
            }
            return results;
        }
    };
    // Tabs on the same session share one read.
    let sessions: Vec<&str> = jobs
        .iter()
        .map(|(_, _, session)| session.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut reads: BTreeMap<&str, GrokRead> = BTreeMap::new();
    let (cache, timeout) = (reader.cache, reader.timeout);
    for batch in sessions.chunks(GROK_PARALLEL_READS) {
        thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|session| {
                    scope.spawn(move || {
                        cache.get(session, force, || {
                            run_grok_usage(executable, session, timeout)
                        })
                    })
                })
                .collect();
            for (session, handle) in batch.iter().zip(handles) {
                let read = handle.join().unwrap_or_else(|_| GrokRead {
                    error: Some("Grok usage read failed".to_owned()),
                    ..GrokRead::default()
                });
                reads.insert(session, read);
            }
        });
    }
    for (shell, pid, session) in &jobs {
        let read = reads.get(session.as_str()).cloned().unwrap_or_default();
        results.insert(
            shell.clone(),
            GrokTabUsage {
                stale: read.usage.is_some() && read.error.is_some(),
                fetched_at_unix: read.fetched_unix,
                usage: read.usage,
                error: read.error,
                checked_at_unix: now,
                pane_pid: Some(*pid),
            },
        );
    }
    results
}

/// A read that could not even identify the session (Grok's list unreadable, tmux
/// failing) says nothing about whether the last figures are still right, so they
/// stay, marked stale, unless the tab's process is known to have changed.
fn keep_stale(
    previous: Option<&GrokTabUsage>,
    pane_pid: Option<u32>,
    error: &str,
    now: u64,
) -> GrokTabUsage {
    let same_process = |previous: &GrokTabUsage| {
        pane_pid.is_none_or(|pid| previous.pane_pid.is_none_or(|before| before == pid))
    };
    match previous.filter(|previous| previous.usage.is_some() && same_process(previous)) {
        Some(previous) => GrokTabUsage {
            stale: true,
            error: Some(error.to_owned()),
            checked_at_unix: now,
            pane_pid: pane_pid.or(previous.pane_pid),
            ..previous.clone()
        },
        None => GrokTabUsage {
            error: Some(error.to_owned()),
            checked_at_unix: now,
            pane_pid,
            ..GrokTabUsage::default()
        },
    }
}

/// The `ProviderUsage` shape for a Grok tab, as `riwork usage` prints it. The
/// account allowance is unknown: only Grok's own `/usage` screen shows it.
pub fn grok_provider_usage(tab: &GrokTabUsage) -> ProviderUsage {
    ProviderUsage {
        provider: "grok".to_owned(),
        windows: Vec::new(),
        updated_at_unix: if tab.fetched_at_unix > 0 {
            tab.fetched_at_unix
        } else {
            unix_now()
        },
        account_label: Some("unknown".to_owned()),
        context_used_percent: None,
        session_cost_usd: tab.usage.as_ref().and_then(|usage| usage.cost_usd),
        session: tab.usage.clone(),
        session_error: tab.error.clone(),
    }
}

/// Session usage for one shell's pane process, read now.
pub fn read_grok_shell_usage(pane_pid: Result<u32, String>) -> GrokTabUsage {
    let target = GrokTarget {
        shell_id: String::new(),
        pane_pid,
    };
    read_grok_usages(&[target], &BTreeMap::new(), true)
        .remove("")
        .unwrap_or_default()
}

/// Tokens and cost of the distinct sessions listed, for a combined figure.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GrokTotals {
    pub sessions: usize,
    pub cost_usd: f64,
    pub tokens: TokenCounts,
}

pub fn grok_totals<'a>(usages: impl IntoIterator<Item = &'a SessionUsage>) -> GrokTotals {
    let mut seen = BTreeSet::new();
    let mut totals = GrokTotals::default();
    for usage in usages {
        // Two tabs can show one session; it is counted once.
        if !seen.insert(usage.session_id.as_str()) {
            continue;
        }
        totals.sessions += 1;
        totals.cost_usd += usage.cost_usd.unwrap_or(0.0);
        let (sum, add) = (&mut totals.tokens, &usage.tokens);
        sum.input = sum.input.saturating_add(add.input);
        sum.output = sum.output.saturating_add(add.output);
        sum.cached_read = sum.cached_read.saturating_add(add.cached_read);
        sum.cache_creation = sum.cache_creation.saturating_add(add.cache_creation);
        sum.reasoning = sum.reasoning.saturating_add(add.reasoning);
        sum.total = sum.total.saturating_add(add.total);
    }
    totals
}

/// The bottom bar's text for a focused Grok tab.
pub fn grok_chip_label(tab: Option<&GrokTabUsage>) -> String {
    let Some(tab) = tab else {
        return "GROK · LOADING".to_owned();
    };
    let Some(usage) = &tab.usage else {
        return "GROK · —".to_owned();
    };
    let cost = usage
        .cost_usd
        .map(|cost| format!(" {}", format_usd(cost)))
        .unwrap_or_default();
    format!(
        "GROK{}{cost} · {} tok",
        if tab.stale { "*" } else { "" },
        format_tokens(usage.tokens.total)
    )
}

/// `950`, `1.2k`, `128k`, `60.7M`.
pub fn format_tokens(count: u64) -> String {
    const UNITS: [&str; 4] = ["", "k", "M", "B"];
    let mut value = count as f64;
    let mut unit = 0;
    while unit + 1 < UNITS.len() && value.round() >= 1000.0 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        return count.to_string();
    }
    let text = if value < 100.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    };
    // 999.9k must not print as 1000k, nor 9.96k as 10.0k.
    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text}{}", UNITS[unit])
}

pub fn format_usd(cost: f64) -> String {
    if cost > 0.0 && cost < 0.005 {
        "<$0.01".to_owned()
    } else {
        format!("${cost:.2}")
    }
}

fn grok_home() -> Result<PathBuf, String> {
    grok_home_from(env::var_os("GROK_HOME"), env::var_os("HOME"))
}

/// `GROK_HOME` when set, else `~/.grok`, as Grok itself resolves its home.
fn grok_home_from(
    grok_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf, String> {
    let path = match grok_home.filter(|path| !path.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(home.ok_or("Cannot locate Grok's home folder")?).join(".grok"),
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("GROK_HOME must be an absolute path".to_owned())
    }
}

/// One line of Grok's `active_sessions.json`: a session its leader considers
/// open, and the process that opened it.
#[derive(Clone, Debug, PartialEq)]
struct ActiveGrokSession {
    session_id: String,
    pid: u32,
    opened_at_unix: Option<f64>,
}

fn read_active_grok_sessions(home: &Path) -> Result<Vec<ActiveGrokSession>, String> {
    let path = home.join("active_sessions.json");
    let read = || -> Result<Option<Vec<ActiveGrokSession>>, String> {
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            // Grok has never run here, or has nothing open.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some(Vec::new()));
            }
            Err(_) => return Err("Cannot read Grok's list of active sessions".to_owned()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_ACTIVE_SESSIONS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read Grok's list of active sessions".to_owned())?;
        if bytes.len() as u64 > MAX_ACTIVE_SESSIONS_BYTES {
            return Err("Grok's list of active sessions is too large".to_owned());
        }
        Ok(parse_active_grok_sessions(&bytes).ok())
    };
    // Grok's leader rewrites this file while sessions open and close, so a
    // read can catch it half written. One more look almost always settles it.
    for attempt in 0..2 {
        if let Some(entries) = read()? {
            return Ok(entries);
        }
        if attempt == 0 {
            thread::sleep(Duration::from_millis(100));
        }
    }
    Err("Grok's list of active sessions is unreadable; try again".to_owned())
}

fn parse_active_grok_sessions(bytes: &[u8]) -> Result<Vec<ActiveGrokSession>, String> {
    let entries: Vec<Value> = serde_json::from_slice(bytes)
        .map_err(|_| "Grok's list of active sessions is invalid".to_owned())?;
    Ok(entries
        .iter()
        .filter_map(|entry| {
            let session_id = entry.get("session_id")?.as_str()?;
            let pid = u32::try_from(entry.get("pid")?.as_u64()?)
                .ok()
                .filter(|pid| *pid > 1)?;
            Some(ActiveGrokSession {
                session_id: valid_grok_session_id(session_id)?.to_owned(),
                pid,
                opened_at_unix: entry
                    .get("opened_at")
                    .and_then(Value::as_str)
                    .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
                    .map(|time| time.timestamp_micros() as f64 / 1e6),
            })
        })
        .collect())
}

/// Session IDs are UUIDs unless a client chose its own with `-s`. They are
/// passed on a command line and printed, so anything odd is refused.
fn valid_grok_session_id(id: &str) -> Option<&str> {
    let valid = (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !id.starts_with(['-', '.']);
    valid.then_some(id)
}

/// What is known about a process without asking anything of it.
struct ProcessFacts {
    executable: Option<PathBuf>,
    started_unix: Option<f64>,
}

trait ProcessProbe: Sync {
    /// `None` when there is no such process.
    fn facts(&self, pid: u32) -> Option<ProcessFacts>;
    fn children(&self, pid: u32) -> Vec<u32>;
}

struct SystemProcesses;

impl ProcessProbe for SystemProcesses {
    fn facts(&self, pid: u32) -> Option<ProcessFacts> {
        let identity = crate::runtime::process_identity(pid).ok().flatten()?;
        Some(ProcessFacts {
            started_unix: identity.started_unix(),
            executable: identity.executable,
        })
    }

    fn children(&self, pid: u32) -> Vec<u32> {
        let mut command = Command::new("ps");
        command
            .args(["-axo", "pid=,ppid="])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        let Ok((status, output)) = run_bounded(command, Duration::from_secs(3), 4 * 1024 * 1024)
        else {
            return Vec::new();
        };
        if !status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                let child = fields.next()?.parse::<u32>().ok()?;
                (fields.next()?.parse::<u32>().ok()? == pid).then_some(child)
            })
            .collect()
    }
}

/// Grok installs as `grok`, or as a versioned file (`grok-1.0.45-macos-aarch64`)
/// that `~/.grok/bin/grok` points to; the kernel reports the latter.
fn is_grok_executable(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|name| name == "grok" || name.starts_with("grok-"))
}

/// The Grok session a tab's pane hosts, or why none can be named. RiWork execs
/// Grok as the pane's process, so that pid is the one Grok lists; only when the
/// pane runs something else (a wrapper) are its direct children considered.
/// Grok's list also keeps entries whose pid has since been reused, so a listed
/// pid counts only while it is a Grok executable that started no later than the
/// entry says. Two different sessions for one tab are refused, not guessed.
fn resolve_grok_session(
    entries: &[ActiveGrokSession],
    pane_pid: u32,
    processes: &dyn ProcessProbe,
) -> Result<String, String> {
    let pane = processes
        .facts(pane_pid)
        .ok_or("This tab's process is no longer running")?;
    let is_grok =
        |facts: &ProcessFacts| facts.executable.as_deref().is_some_and(is_grok_executable);
    let candidates = if is_grok(&pane) {
        vec![pane_pid]
    } else {
        processes.children(pane_pid)
    };
    let mut sessions = BTreeSet::new();
    for pid in candidates {
        let Some(facts) = processes.facts(pid).filter(is_grok) else {
            continue;
        };
        for entry in entries.iter().filter(|entry| entry.pid == pid) {
            let started_in_time = match (facts.started_unix, entry.opened_at_unix) {
                (Some(started), Some(opened)) => started <= opened + GROK_START_SLACK_SECS,
                _ => true,
            };
            if started_in_time {
                sessions.insert(entry.session_id.as_str());
            }
        }
    }
    let mut sessions = sessions.into_iter();
    match (sessions.next(), sessions.next()) {
        (Some(session), None) => Ok(session.to_owned()),
        (None, _) => Err("Grok has not registered a session for this tab yet".to_owned()),
        _ => Err("Grok lists several sessions for this tab, so its usage is not shown".to_owned()),
    }
}

fn run_grok_usage(
    executable: &Path,
    session_id: &str,
    timeout: Duration,
) -> Result<SessionUsage, String> {
    let mut command = Command::new(executable);
    command
        .args(["usage", session_id])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let (status, output) = run_bounded(command, timeout, MAX_GROK_OUTPUT)
        .map_err(|error| format!("Grok usage: {error}"))?;
    if !status.success() {
        return Err(match status.code() {
            Some(code) => format!("Grok could not report this session's usage (exit {code})"),
            None => "Grok could not report this session's usage".to_owned(),
        });
    }
    parse_grok_usage(&output, session_id)
}

/// Run `command` to completion within `timeout`, keeping at most `limit` bytes
/// of its output. The child gets its own process group, which a timeout kills.
/// The reader is waited for only briefly once the child is gone: a descendant
/// that outlives it and keeps the pipe open must not hold this call (and the
/// session's cache slot behind it) forever. Everything the child wrote before
/// exiting has been read by then.
fn run_bounded(
    mut command: Command,
    timeout: Duration,
    limit: u64,
) -> Result<(ExitStatus, Vec<u8>), String> {
    const READER_GRACE: Duration = Duration::from_secs(1);
    command.stdout(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "cannot start the Grok CLI".to_owned())?;
    let stop = |child: &mut Child| {
        #[cfg(unix)]
        if let Some(pid) = i32::try_from(child.id()).ok().filter(|pid| *pid > 1) {
            // SAFETY: this group was created for this child by process_group(0),
            // and the child has not been reaped yet.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    };
    let Some(mut stdout) = child.stdout.take() else {
        stop(&mut child);
        return Err("the Grok CLI has no output".to_owned());
    };
    let output = Arc::new(Mutex::new(Vec::new()));
    let (finished, reader_done) = mpsc::channel();
    let started = thread::Builder::new().spawn({
        let output = Arc::clone(&output);
        move || {
            let mut chunk = [0u8; 8192];
            while let Ok(count @ 1..) = stdout.read(&mut chunk) {
                let mut kept = output.lock().unwrap_or_else(PoisonError::into_inner);
                let room = (limit + 1).saturating_sub(kept.len() as u64) as usize;
                kept.extend_from_slice(&chunk[..count.min(room)]);
                if kept.len() as u64 > limit {
                    break;
                }
            }
            let _ = finished.send(());
        }
    });
    if started.is_err() {
        stop(&mut child);
        return Err("cannot read the Grok CLI's output".to_owned());
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => break None,
        }
    };
    let Some(status) = status else {
        stop(&mut child);
        let _ = reader_done.recv_timeout(READER_GRACE);
        return Err("the read timed out; try again".to_owned());
    };
    let _ = reader_done.recv_timeout(READER_GRACE);
    let output = std::mem::take(&mut *output.lock().unwrap_or_else(PoisonError::into_inner));
    if output.len() as u64 > limit {
        return Err("the report is too large".to_owned());
    }
    Ok((status, output))
}

/// Parse `grok usage <session>`. Grok's leader writes session files as it
/// works, so a read can be partial: anything that is not a complete report for
/// exactly this session is an error, never a zero.
fn parse_grok_usage(bytes: &[u8], session_id: &str) -> Result<SessionUsage, String> {
    let invalid = || "Grok returned unreadable usage".to_owned();
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if value.get("sessionId").and_then(Value::as_str) != Some(session_id) {
        return Err("Grok returned usage for a different session".to_owned());
    }
    let session = value
        .get("session")
        .filter(|session| session.is_object())
        .ok_or_else(invalid)?;
    let totals = grok_totals_at(session)?.ok_or_else(invalid)?;
    let turns = json_count(session, "turnCount")?.unwrap_or(0);
    // Nothing counted at all is a session Grok has not accounted yet, and zero
    // would read as free.
    if totals.tokens == TokenCounts::default()
        && totals.model_calls == 0
        && totals.cost_usd.is_none_or(|cost| cost == 0.0)
        && turns == 0
    {
        return Err("Grok has no usage recorded for this session yet".to_owned());
    }
    let mut models = Vec::new();
    if let Some(usage) = session.get("modelUsage").and_then(Value::as_object) {
        for (name, model) in usage.iter().take(64) {
            let name = clean_label(name);
            if let (false, Ok(Some(totals))) = (name.is_empty(), grok_totals_at(model)) {
                models.push(ModelUsage {
                    model: name,
                    model_calls: totals.model_calls,
                    tokens: totals.tokens,
                    cost_usd: totals.cost_usd,
                });
            }
        }
    }
    models.sort_by(|a, b| {
        b.tokens
            .total
            .cmp(&a.tokens.total)
            .then_with(|| a.model.cmp(&b.model))
    });
    let primary_model = session
        .get("primaryModelId")
        .and_then(Value::as_str)
        .map(clean_label)
        .filter(|name| !name.is_empty())
        .or_else(|| models.first().map(|model| model.model.clone()));
    let updated = value
        .get("updatedAt")
        .and_then(Value::as_str)
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok());
    Ok(SessionUsage {
        session_id: session_id.to_owned(),
        updated_at: updated.map(|time| {
            time.to_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        }),
        updated_at_unix: updated.and_then(|time| u64::try_from(time.timestamp()).ok()),
        primary_model,
        turns,
        model_calls: totals.model_calls,
        tokens: totals.tokens,
        cost_usd: totals.cost_usd,
        models,
    })
}

struct GrokTotalsAt {
    tokens: TokenCounts,
    model_calls: u64,
    cost_usd: Option<f64>,
}

/// The token, call and cost fields Grok reports for a session, a turn or a
/// model. `None` when none of them are present.
fn grok_totals_at(value: &Value) -> Result<Option<GrokTotalsAt>, String> {
    let input = json_count(value, "inputTokens")?;
    let output = json_count(value, "outputTokens")?;
    let total = json_count(value, "totalTokens")?;
    let ticks = json_count(value, "costUsdTicks")?;
    if input.is_none() && output.is_none() && total.is_none() && ticks.is_none() {
        return Ok(None);
    }
    let (input, output) = (input.unwrap_or(0), output.unwrap_or(0));
    Ok(Some(GrokTotalsAt {
        tokens: TokenCounts {
            input,
            output,
            cached_read: json_count(value, "cachedReadTokens")?.unwrap_or(0),
            cache_creation: json_count(value, "cacheCreationTokens")?.unwrap_or(0),
            reasoning: json_count(value, "reasoningTokens")?.unwrap_or(0),
            total: total.unwrap_or_else(|| input.saturating_add(output)),
        },
        model_calls: json_count(value, "modelCalls")?.unwrap_or(0),
        cost_usd: ticks.map(|ticks| ticks as f64 / GROK_TICKS_PER_USD),
    }))
}

/// A non-negative whole number, absent, or an error for anything else.
fn json_count(value: &Value, key: &str) -> Result<Option<u64>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(number) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| "Grok returned unreadable usage".to_owned()),
    }
}

/// What the last attempt to read a Grok session left.
#[derive(Clone, Debug, Default)]
struct GrokRead {
    usage: Option<SessionUsage>,
    fetched_unix: u64,
    error: Option<String>,
}

#[derive(Default)]
struct GrokCacheEntry {
    read: GrokRead,
    attempted: Option<Instant>,
}

/// Reads of `grok usage` by session, shared by everything in the process. A
/// failed read keeps the last good figures, which are then reported stale.
struct GrokUsageCache {
    ttl: Duration,
    sessions: Mutex<BTreeMap<String, Arc<Mutex<GrokCacheEntry>>>>,
}

impl GrokUsageCache {
    const fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    /// The session's usage from a read no older than the TTL, else from `fetch`.
    /// `force` reads again unless another caller's read finished after this one
    /// asked. Callers for one session wait for the read in progress; different
    /// sessions do not wait for each other.
    fn get(
        &self,
        session_id: &str,
        force: bool,
        fetch: impl FnOnce() -> Result<SessionUsage, String>,
    ) -> GrokRead {
        let asked = Instant::now();
        let slot = {
            let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            if sessions.len() >= GROK_CACHE_LIMIT && !sessions.contains_key(session_id) {
                sessions.clear();
            }
            sessions.entry(session_id.to_owned()).or_default().clone()
        };
        let mut entry = slot.lock().unwrap_or_else(PoisonError::into_inner);
        // A glitch after good figures (Grok mid-write) is worth trying again
        // soon. A session that has never read (no turns yet, an unsupported
        // Grok) waits the full TTL, so it costs no more than a healthy one.
        let wait = if entry.read.error.is_some() && entry.read.usage.is_some() {
            self.ttl.min(GROK_RETRY_AFTER_FAILURE)
        } else {
            self.ttl
        };
        let current = entry
            .attempted
            .is_some_and(|at| at >= asked || (!force && at.elapsed() < wait));
        if !current {
            match fetch() {
                Ok(usage) => {
                    entry.read = GrokRead {
                        usage: Some(usage),
                        fetched_unix: unix_now(),
                        error: None,
                    };
                }
                Err(error) => entry.read.error = Some(error),
            }
            entry.attempted = Some(Instant::now());
        }
        entry.read.clone()
    }
}

fn usage_home() -> Result<PathBuf, String> {
    crate::paths::riwork_home()
}

fn cache_path(home: &Path, shell_id: &str) -> Result<PathBuf, String> {
    let id = Uuid::parse_str(shell_id).map_err(|_| "Shell ID must be a full UUID")?;
    Ok(home.join("usage").join(format!("{id}.json")))
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|value| valid_number(*value))
}

fn number_at(value: &Value, pointer: &str) -> Option<f64> {
    value.pointer(pointer).and_then(number)
}

fn valid_number(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

fn positive_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64).filter(|value| *value > 0)
}

fn clean_label(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(120)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn find_codex() -> Option<PathBuf> {
    let mut directories: Vec<_> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    directories.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        directories.extend([
            home.join(".local/bin"),
            home.join(".bun/bin"),
            home.join(".npm-global/bin"),
        ]);
    }
    directories
        .into_iter()
        .map(|directory| directory.join("codex"))
        .find(|path| path.is_file())
}

struct CodexServer {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<Value>,
}

impl CodexServer {
    fn start(executable: &Path, home: Option<&Path>) -> Result<Self, String> {
        let mut command = Command::new(executable);
        command
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(home) = home {
            // Override an inherited account binding only for this read. Passing
            // the path as an environment value also preserves spaces verbatim.
            command.env("CODEX_HOME", home);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // npm-installed Codex can be a wrapper that spawns the native
            // binary. Give this read its own group so cleanup includes both.
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|_| "Cannot start the Codex usage reader")?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or("Codex usage reader has no output")?;
        let (send, messages) = mpsc::sync_channel(32);
        thread::spawn(move || {
            for line in BufReader::new(stdout.take(MAX_SERVER_OUTPUT)).lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if send.send(value).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            messages,
        })
    }

    fn send(&mut self, value: Value) -> Result<(), String> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or("Codex usage reader input is closed")?;
        serde_json::to_writer(&mut *stdin, &value)
            .map_err(|_| "Cannot send a Codex usage request")?;
        stdin
            .write_all(b"\n")
            .and_then(|_| stdin.flush())
            .map_err(|_| "Cannot send a Codex usage request".into())
    }

    fn next(&self, deadline: Instant) -> Result<Value, String> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match self.messages.recv_timeout(remaining) {
            Ok(value) => Ok(value),
            Err(RecvTimeoutError::Timeout) => Err("Codex usage read timed out; try again".into()),
            Err(RecvTimeoutError::Disconnected) => {
                Err("Codex usage reader exited; check its sign-in and installed version".into())
            }
        }
    }

    fn response(&self, id: u64, deadline: Instant) -> Result<Value, String> {
        loop {
            let message = self.next(deadline)?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                return response_result(&message);
            }
        }
    }
}

impl Drop for CodexServer {
    fn drop(&mut self) {
        self.stdin.take();
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            if let Ok(pid) = i32::try_from(self.child.id()) {
                // SAFETY: this process group was created exclusively for this
                // child by process_group(0), and Child has not been reaped yet.
                unsafe {
                    kill(-pid, 9);
                }
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn response_result(message: &Value) -> Result<Value, String> {
    if let Some(error) = message.get("error") {
        if error.get("code").and_then(Value::as_i64) == Some(-32601) {
            return Err("Installed Codex does not support subscription usage; update Codex".into());
        }
        // Server error text can contain details from auth/network responses.
        // Expose a fixed message rather than copying it to the UI or CLI.
        return Err(
            "Codex could not read subscription usage; check its sign-in or try again".into(),
        );
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| "Codex returned an invalid usage response".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failing_codex_reads_back_off_exponentially_up_to_a_cap() {
        let waits: Vec<_> = (0..9).map(|n| codex_refresh_interval(false, n)).collect();
        assert_eq!(waits[..5], [60, 120, 240, 480, 600]);
        assert!(waits[5..].iter().all(|wait| *wait == 10 * 60));
        // A cached snapshot keeps its normal cadence until refreshes start failing.
        assert_eq!(codex_refresh_interval(true, 0), 15 * 60);
        assert_eq!(codex_refresh_interval(true, 1), 30 * 60);
        assert_eq!(codex_refresh_interval(true, u32::MAX), 30 * 60);
        assert_eq!(codex_refresh_interval(false, u32::MAX), 10 * 60);
    }

    #[test]
    fn codex_uses_reported_durations_and_not_assumed_windows() {
        let usage = parse_codex_usage(
            &json!({"rateLimits": {
                "primary": {"usedPercent": 23, "windowDurationMins": 180, "resetsAt": 5000},
                "secondary": {"usedPercent": 41, "windowDurationMins": 1440, "resetsAt": null}
            }}),
            Some("Pro".into()),
            123,
        )
        .unwrap();
        assert_eq!(usage.windows[0].label, "3h");
        assert_eq!(usage.windows[0].window_minutes, Some(180));
        assert_eq!(usage.windows[0].resets_at, Some(5000));
        assert_eq!(usage.windows[1].label, "1d");
        assert_eq!(usage.windows[1].resets_at, None);
        assert_eq!(usage.account_label.as_deref(), Some("Pro"));
        assert_eq!(usage.updated_at_unix, 123);
    }

    #[test]
    fn codex_multibucket_avoids_mirrored_legacy_and_preserves_names() {
        let window = json!({"usedPercent": 12, "windowDurationMins": 300});
        let usage = parse_codex_usage(
            &json!({
                "rateLimits": {"primary": window.clone()},
                "rateLimitsByLimitId": {
                    "codex": {"primary": window.clone()},
                    "spark": {"limitName": "Spark", "primary": window}
                }
            }),
            None,
            1,
        )
        .unwrap();
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].label, "5h");
        assert_eq!(usage.windows[1].label, "Spark 5h");
    }

    #[test]
    fn codex_missing_and_malformed_windows_are_not_zero_usage() {
        assert!(parse_codex_usage(&json!({"rateLimits": {"primary": null}}), None, 1).is_err());
        assert!(
            parse_codex_usage(
                &json!({"rateLimits": {"primary": {"usedPercent": "12"}}}),
                None,
                1
            )
            .is_err()
        );
        let usage = parse_codex_usage(
            &json!({"rateLimits": {"primary": {
                "usedPercent": 0, "windowDurationMins": -3, "resetsAt": -1
            }}}),
            None,
            1,
        )
        .unwrap();
        assert_eq!(usage.windows[0].label, "Primary");
        assert_eq!(usage.windows[0].resets_at, None);
        assert_eq!(usage.windows[0].window_minutes, None);
    }

    #[test]
    fn claude_absent_quota_preserves_context_without_inventing_usage() {
        let usage = parse_claude_usage(
            &json!({
                "context_window": {"used_percentage": 18.5},
                "cost": {"total_cost_usd": 0.125}
            }),
            42,
        )
        .unwrap();
        assert!(usage.windows.is_empty());
        assert_eq!(usage.context_used_percent, Some(18.5));
        assert_eq!(usage.session_cost_usd, Some(0.125));
        assert!(parse_claude_usage(&json!(null), 42).is_err());
    }

    #[test]
    fn claude_partial_and_spend_windows_remain_independent() {
        let usage = parse_claude_usage(
            &json!({"rate_limits": {
                "five_hour": {"used_percentage": 29.5, "resets_at": 900},
                "seven_day": {"used_percentage": null},
                "spend_limit": {"used_percentage": 101, "resets_at": 1200}
            }}),
            1,
        )
        .unwrap();
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].window_minutes, Some(300));
        assert_eq!(usage.windows[1].window_minutes, None);
        assert_eq!(usage.windows[1].used_percent, 101.0);
    }

    #[test]
    fn claude_cache_only_contains_sanitized_metrics_and_is_shell_scoped() {
        let home = env::temp_dir().join(format!("riwork-usage-test-{}", Uuid::new_v4()));
        let id = Uuid::new_v4().to_string();
        assert!(read_claude_usage_at(&home, &id).unwrap().is_none());
        let status = json!({
            "model": {"display_name": "Opus\u{001b}"},
            "context_window": {"used_percentage": 17},
            "cost": {"total_cost_usd": 0.7},
            "rate_limits": {"seven_day": {"used_percentage": 45, "resets_at": 8000}},
            "transcript_path": "/private/transcript.jsonl", "unknown_credentials": "never-store-this"
        });
        record_claude_status_at(&home, &id, &status).unwrap();
        let restored = read_claude_usage_at(&home, &id).unwrap().unwrap();
        assert_eq!(restored.windows[0].used_percent, 45.0);
        assert_eq!(restored.context_used_percent, Some(17.0));
        let path = cache_path(&home, &id).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("transcript"));
        assert!(!raw.contains("never-store-this"));
        assert_eq!(
            claude_status_text(&status),
            "Opus · CTX 17% · EST $0.70 · 7d 45% used"
        );
        assert!(cache_path(&home, "../../escape").is_err());
        fs::remove_file(path).unwrap();
        fs::remove_dir(home.join("usage")).unwrap();
        fs::remove_dir(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn codex_stdio_accepts_out_of_order_replies_and_times_out_cleanly() {
        use std::os::unix::fs::PermissionsExt;
        let home = env::temp_dir().join(format!("riwork-usage-rpc-{}", Uuid::new_v4()));
        fs::create_dir(&home).unwrap();
        let executable = home.join("codex-fixture");
        fs::write(&executable, r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"id":1,"result":{}}' ;;
    *'"method":"account/rateLimits/read"'*)
      printf '%s\n' '{"id":3,"result":{"rateLimits":{"primary":{"usedPercent":8,"windowDurationMins":120,"resetsAt":5000}}}}'
      printf '%s\n' '{"id":2,"result":{"account":{"type":"chatgpt","email":"fixture@example.test","planType":"pro"}}}' ;;
  esac
done
"#).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let usage = read_codex_usage_with(&executable, None, Duration::from_secs(2)).unwrap();
        assert_eq!(usage.windows[0].label, "2h");
        assert_eq!(usage.windows[0].used_percent, 8.0);
        assert_eq!(
            usage.account_label.as_deref(),
            Some("fixture@example.test · pro")
        );

        // This fixture consumes stdin but never responds. Its own dedicated
        // group is killed and reaped when the bounded read times out.
        fs::write(
            &executable,
            "#!/bin/sh\nwhile IFS= read -r line; do :; done\n",
        )
        .unwrap();
        let started = Instant::now();
        let error =
            read_codex_usage_with(&executable, None, Duration::from_millis(100)).unwrap_err();
        assert!(error.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
        fs::remove_file(executable).unwrap();
        fs::remove_dir(home).unwrap();
    }

    #[cfg(unix)]
    struct FakeCodex {
        directory: PathBuf,
        executable: PathBuf,
    }

    #[cfg(unix)]
    impl FakeCodex {
        fn new(script: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let directory = env::temp_dir().join(format!("riwork-usage-home-{}", Uuid::new_v4()));
            fs::create_dir(&directory).unwrap();
            let executable = directory.join("codex fixture");
            fs::write(&executable, script).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                directory,
                executable,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for FakeCodex {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[cfg(unix)]
    #[test]
    fn codex_usage_forces_each_supplied_home_without_changing_parent_environment() {
        let fixture = FakeCodex::new(
            r#"#!/bin/sh
record_dir=${0%/*}
printf '%s' "$CODEX_HOME" > "$record_dir/received-home"
: > "$record_dir/requests"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$record_dir/requests"
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"id":1,"result":{}}' ;;
    *'"method":"account/read"'*)
      printf '%s\n' '{"id":2,"result":{"account":{"type":"chatgpt","email":"fixture@example.test","planType":"pro"}}}' ;;
    *'"method":"account/rateLimits/read"'*)
      printf '%s\n' '{"id":3,"result":{"rateLimits":{"primary":{"usedPercent":8,"windowDurationMins":120}}}}' ;;
  esac
done
"#,
        );
        let inherited_home = env::var_os("CODEX_HOME");
        for name in ["Account One", "Account Two"] {
            let home = fixture.directory.join(name);
            let usage =
                read_codex_usage_with(&fixture.executable, Some(&home), Duration::from_secs(2))
                    .unwrap();
            assert_eq!(
                fs::read_to_string(fixture.directory.join("received-home")).unwrap(),
                home.to_string_lossy()
            );
            assert_eq!(env::var_os("CODEX_HOME"), inherited_home);
            assert_eq!(
                usage.account_label.as_deref(),
                Some("fixture@example.test · pro")
            );
            assert_eq!(usage.windows[0].used_percent, 8.0);

            let requests: Vec<Value> = fs::read_to_string(fixture.directory.join("requests"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(
                requests
                    .iter()
                    .map(|request| request["method"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                [
                    "initialize",
                    "initialized",
                    "account/read",
                    "account/rateLimits/read"
                ]
            );
            assert_eq!(requests[2]["params"]["refreshToken"], false);
        }
    }

    #[cfg(unix)]
    #[test]
    fn account_scoped_usage_errors_never_expose_server_auth_details() {
        for (code, expected) in [
            (
                -32000,
                "Codex could not read subscription usage; check its sign-in or try again",
            ),
            (
                -32601,
                "Installed Codex does not support subscription usage; update Codex",
            ),
        ] {
            let response = json!({"id": 3, "error": {"code": code, "message": "Bearer never-expose-this-token", "data": {"private_path": "/private/auth.json", "email": "private@example.test"}}}).to_string();
            let script = r#"#!/bin/sh
printf '%s\n' 'never-expose-this-stderr' >&2
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"id":1,"result":{}}' ;;
    *'"method":"account/read"'*)
      printf '%s\n' '{"id":2,"error":{"code":-32000,"message":"never-expose-account-auth"}}' ;;
    *'"method":"account/rateLimits/read"'*)
      printf '%s\n' '__ERROR_RESPONSE__' ;;
  esac
done
"#
            .replace("__ERROR_RESPONSE__", &response);
            let fixture = FakeCodex::new(&script);
            let home = fixture.directory.join("private account home");
            let error =
                read_codex_usage_with(&fixture.executable, Some(&home), Duration::from_secs(2))
                    .unwrap_err();
            assert_eq!(error, expected);
            assert!(!error.contains("never-expose"));
            assert!(!error.contains("auth.json"));
            assert!(!error.contains("private@example.test"));
            assert!(!error.contains(home.to_str().unwrap()));
        }
    }

    fn tab(usage: Option<SessionUsage>, stale: bool, error: Option<&str>) -> GrokTabUsage {
        GrokTabUsage {
            fetched_at_unix: if usage.is_some() { 100 } else { 0 },
            usage,
            stale,
            error: error.map(str::to_owned),
            checked_at_unix: 100,
            pane_pid: None,
        }
    }

    const SESSION: &str = "01a0ebde-d244-7bc3-8222-9d1a4330cd15";
    const OTHER_SESSION: &str = "01a0ebd0-7000-7000-8000-000000000001";

    /// The shape `grok usage <id>` printed for a real session (Grok 1.0.45),
    /// with two models so the breakdown is exercised.
    fn grok_report(session: &str) -> Value {
        json!({
            "sessionId": session,
            "updatedAt": "2026-09-29T13:02:35.793940+00:00",
            "session": {
                "inputTokens": 60_210_005u64, "outputTokens": 491_382u64,
                "cachedReadTokens": 56_722_048u64, "cacheCreationTokens": 12u64,
                "reasoningTokens": 346_297u64, "totalTokens": 60_701_387u64,
                "modelCalls": 470u64, "costUsdTicks": 266_177_119_200u64,
                "turnCount": 10u64, "primaryModelId": "grok-4.7-build-fast",
                "modelUsage": {
                    "grok-4.7-build-fast": {
                        "inputTokens": 60_000_000u64, "outputTokens": 400_000u64,
                        "cachedReadTokens": 56_000_000u64, "cacheCreationTokens": 0u64,
                        "reasoningTokens": 300_000u64, "totalTokens": 60_400_000u64,
                        "modelCalls": 460u64, "costUsdTicks": 260_000_000_000u64
                    },
                    "grok-code-fast": {
                        "inputTokens": 210_005u64, "outputTokens": 91_382u64,
                        "cachedReadTokens": 722_048u64, "cacheCreationTokens": 12u64,
                        "reasoningTokens": 46_297u64, "totalTokens": 301_387u64,
                        "modelCalls": 10u64, "costUsdTicks": 6_177_119_200u64
                    }
                }
            },
            "turns": [{"turnNumber": 2, "endedAt": "2026-09-29T10:14:20.186116+00:00",
                       "inputTokens": 1u64, "totalTokens": 1u64}]
        })
    }

    #[test]
    fn grok_usage_report_is_parsed_with_a_per_model_breakdown() {
        let usage = parse_grok_usage(grok_report(SESSION).to_string().as_bytes(), SESSION).unwrap();
        assert_eq!(usage.session_id, SESSION);
        assert_eq!(usage.turns, 10);
        assert_eq!(usage.model_calls, 470);
        assert_eq!(
            usage.tokens,
            TokenCounts {
                input: 60_210_005,
                output: 491_382,
                cached_read: 56_722_048,
                cache_creation: 12,
                reasoning: 346_297,
                total: 60_701_387,
            }
        );
        assert_eq!(usage.primary_model.as_deref(), Some("grok-4.7-build-fast"));
        assert_eq!(usage.updated_at.as_deref(), Some("2026-09-29T13:02:35Z"));
        assert_eq!(usage.updated_at_unix, Some(1_790_686_955));
        assert!((usage.cost_usd.unwrap() - 26.61771192).abs() < 1e-9);
        // Largest model first, each with its own tokens, calls and cost.
        assert_eq!(usage.models.len(), 2);
        assert_eq!(usage.models[0].model, "grok-4.7-build-fast");
        assert_eq!(usage.models[0].model_calls, 460);
        assert_eq!(usage.models[0].cost_usd, Some(26.0));
        assert_eq!(usage.models[1].model, "grok-code-fast");
        assert_eq!(usage.models[1].tokens.total, 301_387);
        // The per-turn list is not kept.
        assert!(
            !serde_json::to_string(&usage)
                .unwrap()
                .contains("turnNumber")
        );
    }

    #[test]
    fn grok_totals_default_and_model_fallbacks() {
        // Total is input + output when Grok omits it; the largest model stands
        // in for a missing primary model; absent counters are zero.
        let usage = parse_grok_usage(
            json!({
                "sessionId": SESSION,
                "session": {
                    "inputTokens": 7, "outputTokens": 5,
                    "modelUsage": {"small": {"totalTokens": 1}, "big": {"totalTokens": 9}}
                }
            })
            .to_string()
            .as_bytes(),
            SESSION,
        )
        .unwrap();
        assert_eq!(usage.tokens.total, 12);
        assert_eq!(usage.tokens.cached_read, 0);
        assert_eq!(usage.turns, 0);
        assert_eq!(usage.cost_usd, None);
        assert_eq!(usage.updated_at, None);
        assert_eq!(usage.primary_model.as_deref(), Some("big"));
    }

    #[test]
    fn grok_cost_ticks_convert_at_ten_billion_per_dollar() {
        let cost = |ticks: Value| {
            let report =
                json!({"sessionId": SESSION, "session": {"totalTokens": 1, "costUsdTicks": ticks}});
            parse_grok_usage(report.to_string().as_bytes(), SESSION).map(|usage| usage.cost_usd)
        };
        assert_eq!(cost(json!(10_000_000_000u64)).unwrap(), Some(1.0));
        assert_eq!(cost(json!(4_200_000_000u64)).unwrap(), Some(0.42));
        assert_eq!(cost(json!(0)).unwrap(), Some(0.0));
        assert_eq!(cost(json!(1)).unwrap(), Some(1e-10));
        assert!(cost(json!(u64::MAX)).unwrap().unwrap().is_finite());
        assert_eq!(cost(json!(null)).unwrap(), None);
        assert!(cost(json!(-5)).is_err());
        assert!(cost(json!("7")).is_err());
        assert!(cost(json!(1.5)).is_err());
    }

    #[test]
    fn partial_or_garbage_grok_reports_are_errors_and_never_zero_usage() {
        let good = grok_report(SESSION).to_string();
        let mut cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"not json at all".to_vec(),
            b"<html>502</html>".to_vec(),
            b"[]".to_vec(),
            b"null".to_vec(),
            b"{}".to_vec(),
            // Cut off mid-write at every length a reader could catch.
            good.as_bytes()[..good.len() / 2].to_vec(),
            good.as_bytes()[..good.len() - 1].to_vec(),
            json!({"sessionId": SESSION}).to_string().into_bytes(),
            json!({"sessionId": SESSION, "session": []})
                .to_string()
                .into_bytes(),
            // An object that reports nothing measurable is not a zero report.
            json!({"sessionId": SESSION, "session": {"primaryModelId": "x"}})
                .to_string()
                .into_bytes(),
            json!({"sessionId": SESSION, "session": {"inputTokens": "many"}})
                .to_string()
                .into_bytes(),
            json!({"sessionId": SESSION, "session": {"inputTokens": -1}})
                .to_string()
                .into_bytes(),
            json!({"sessionId": SESSION, "session": {"inputTokens": 1, "modelCalls": {}}})
                .to_string()
                .into_bytes(),
            // Another session's numbers must not be shown as this one's.
            grok_report(OTHER_SESSION).to_string().into_bytes(),
            json!({"session": {"totalTokens": 1}})
                .to_string()
                .into_bytes(),
            vec![0xff, 0xfe, 0x00],
        ];
        cases.extend(
            (1..good.len())
                .step_by(97)
                .map(|cut| good.as_bytes()[..cut].to_vec()),
        );
        for case in cases {
            let result = parse_grok_usage(&case, SESSION);
            assert!(
                result.is_err(),
                "accepted {:?}: {result:?}",
                String::from_utf8_lossy(&case[..case.len().min(80)])
            );
        }
        // A model entry with garbage is dropped; the session totals stand.
        let usage = parse_grok_usage(
            json!({"sessionId": SESSION, "session": {"totalTokens": 3,
                "modelUsage": {"ok": {"totalTokens": 3}, "bad": {"totalTokens": "x"}, "empty": {}}}})
            .to_string()
            .as_bytes(),
            SESSION,
        )
        .unwrap();
        assert_eq!(usage.models.len(), 1);
    }

    #[test]
    fn active_grok_sessions_skip_bad_entries_and_reject_garbage() {
        let entries = parse_active_grok_sessions(
            json!([
                {"session_id": SESSION, "pid": 4242, "cwd": "/w",
                 "opened_at": "2026-09-29T00:32:00.914034Z"},
                {"session_id": "no-pid", "cwd": "/w"},
                {"session_id": "../etc", "pid": 5},
                {"session_id": "-flag", "pid": 6},
                {"session_id": "", "pid": 7},
                {"session_id": OTHER_SESSION, "pid": 1},
                {"session_id": OTHER_SESSION, "pid": -3},
                {"session_id": OTHER_SESSION, "pid": 99_999_999_999u64},
                {"session_id": OTHER_SESSION, "pid": 4243, "opened_at": "yesterday"},
                "text", null, 7
            ])
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].pid, 4242);
        assert_eq!(entries[0].opened_at_unix, Some(1_790_641_920.914034));
        assert_eq!((entries[1].pid, entries[1].opened_at_unix), (4243, None));
        for garbage in ["", "{", "{\"pid\":1}", "[{\"session_id\":", "nope"] {
            assert!(
                parse_active_grok_sessions(garbage.as_bytes()).is_err(),
                "{garbage}"
            );
        }
        assert!(parse_active_grok_sessions(b"[]").unwrap().is_empty());
    }

    #[test]
    fn grok_home_follows_grok_home_then_home() {
        use std::ffi::OsString;
        let some = |text: &str| Some(OsString::from(text));
        assert_eq!(
            grok_home_from(some("/custom"), some("/Users/x")).unwrap(),
            PathBuf::from("/custom")
        );
        assert_eq!(
            grok_home_from(None, some("/Users/x")).unwrap(),
            PathBuf::from("/Users/x/.grok")
        );
        assert_eq!(
            grok_home_from(some(""), some("/Users/x")).unwrap(),
            PathBuf::from("/Users/x/.grok")
        );
        assert!(grok_home_from(some("relative"), some("/Users/x")).is_err());
        assert!(grok_home_from(None, None).is_err());
    }

    #[derive(Default)]
    struct FakeProcesses {
        facts: BTreeMap<u32, (String, Option<f64>)>,
        children: BTreeMap<u32, Vec<u32>>,
    }

    impl FakeProcesses {
        fn with(mut self, pid: u32, executable: &str, started: Option<f64>) -> Self {
            self.facts.insert(pid, (executable.to_owned(), started));
            self
        }

        fn child(mut self, parent: u32, child: u32) -> Self {
            self.children.entry(parent).or_default().push(child);
            self
        }
    }

    impl ProcessProbe for FakeProcesses {
        fn facts(&self, pid: u32) -> Option<ProcessFacts> {
            self.facts
                .get(&pid)
                .map(|(executable, started)| ProcessFacts {
                    executable: Some(PathBuf::from(executable)),
                    started_unix: *started,
                })
        }

        fn children(&self, pid: u32) -> Vec<u32> {
            self.children.get(&pid).cloned().unwrap_or_default()
        }
    }

    fn listed(session: &str, pid: u32, opened_at: Option<f64>) -> ActiveGrokSession {
        ActiveGrokSession {
            session_id: session.to_owned(),
            pid,
            opened_at_unix: opened_at,
        }
    }

    const GROK: &str = "/Users/x/.grok/downloads/grok-1.0.45-macos-aarch64";

    #[test]
    fn a_tab_maps_to_the_session_listed_for_its_pane_process() {
        let entries = [
            listed(OTHER_SESSION, 3000, Some(1_000.0)),
            listed(SESSION, 4000, Some(1_000.0)),
        ];
        let processes =
            FakeProcesses::default()
                .with(3000, GROK, Some(999.0))
                .with(4000, GROK, Some(999.0));
        assert_eq!(
            resolve_grok_session(&entries, 4000, &processes).unwrap(),
            SESSION
        );
        assert_eq!(
            resolve_grok_session(&entries, 3000, &processes).unwrap(),
            OTHER_SESSION
        );
        // Plain `grok` and the versioned file name both count.
        for name in [
            "/Users/x/.grok/bin/grok",
            "/opt/bin/GROK",
            "/a/grok-1.0.43-macos-aarch64",
        ] {
            assert!(is_grok_executable(Path::new(name)), "{name}");
        }
        for name in [
            "/bin/zsh",
            "/usr/bin/node",
            "/a/grokker",
            "/a/ungrok",
            "/a/grok/zsh",
            "",
        ] {
            assert!(!is_grok_executable(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn a_wrapper_pane_uses_the_listed_grok_among_its_direct_children_only() {
        let entries = [
            listed(SESSION, 4001, Some(1_000.0)),
            // A grandchild is not the pane's Grok.
            listed(OTHER_SESSION, 4002, Some(1_000.0)),
        ];
        let processes = FakeProcesses::default()
            .with(4000, "/bin/zsh", Some(900.0))
            .with(4001, GROK, Some(999.0))
            .with(4002, GROK, Some(999.0))
            .with(4003, "/usr/bin/caffeinate", Some(999.0))
            .child(4000, 4001)
            .child(4000, 4003)
            .child(4001, 4002);
        assert_eq!(
            resolve_grok_session(&entries, 4000, &processes).unwrap(),
            SESSION
        );
        // No Grok child at all.
        let processes = FakeProcesses::default()
            .with(4000, "/bin/zsh", Some(900.0))
            .with(4003, "/usr/bin/caffeinate", Some(999.0))
            .child(4000, 4003);
        assert!(resolve_grok_session(&entries, 4000, &processes).is_err());
    }

    #[test]
    fn a_grok_pane_never_borrows_a_session_from_its_own_children() {
        // The pane process is Grok and is not listed (yet); a Grok helper it
        // started is listed. That session is not this tab's.
        let entries = [listed(OTHER_SESSION, 4001, Some(1_000.0))];
        let processes = FakeProcesses::default()
            .with(4000, GROK, Some(999.0))
            .with(4001, GROK, Some(999.0))
            .child(4000, 4001);
        let error = resolve_grok_session(&entries, 4000, &processes).unwrap_err();
        assert!(error.contains("not registered"), "{error}");
    }

    #[test]
    fn stale_entries_with_a_reused_pid_are_rejected() {
        let entries = [listed(SESSION, 4000, Some(1_000.0))];
        let outcome = |processes: FakeProcesses| resolve_grok_session(&entries, 4000, &processes);

        // The pid now belongs to an unrelated program.
        for other in [
            "/bin/zsh",
            "/usr/bin/node",
            "/Applications/Xcode.app/Contents/MacOS/Xcode",
        ] {
            let error =
                outcome(FakeProcesses::default().with(4000, other, Some(1_000.0))).unwrap_err();
            assert!(error.contains("not registered"), "{other}: {error}");
        }
        // A Grok that started long after the entry was written is a different
        // process that reused the pid.
        assert!(outcome(FakeProcesses::default().with(4000, GROK, Some(1_061.0))).is_err());
        assert!(outcome(FakeProcesses::default().with(4000, GROK, Some(9_000.0))).is_err());
        // Started before the entry, or within the clock slack: this is it.
        for started in [0.0, 999.0, 1_000.0, 1_059.0] {
            assert_eq!(
                outcome(FakeProcesses::default().with(4000, GROK, Some(started))).unwrap(),
                SESSION,
                "started {started}"
            );
        }
        // Where the platform gives no start time, the executable decides.
        assert_eq!(
            outcome(FakeProcesses::default().with(4000, GROK, None)).unwrap(),
            SESSION
        );
        assert!(outcome(FakeProcesses::default().with(4000, "/bin/zsh", None)).is_err());
        // The process is gone entirely.
        let error = outcome(FakeProcesses::default()).unwrap_err();
        assert!(error.contains("no longer running"), "{error}");
        // No usable opened_at: the executable still has to be Grok.
        let unstamped = [listed(SESSION, 4000, None)];
        let grok = FakeProcesses::default().with(4000, GROK, Some(5.0));
        assert_eq!(
            resolve_grok_session(&unstamped, 4000, &grok).unwrap(),
            SESSION
        );
    }

    #[test]
    fn ambiguous_grok_entries_fail_closed() {
        let processes = FakeProcesses::default().with(4000, GROK, Some(999.0));
        // One process listed under two sessions.
        let entries = [
            listed(SESSION, 4000, Some(1_000.0)),
            listed(OTHER_SESSION, 4000, Some(1_000.0)),
        ];
        let error = resolve_grok_session(&entries, 4000, &processes).unwrap_err();
        assert!(error.contains("several sessions"), "{error}");
        // The same session twice is not ambiguous.
        let twice = [
            listed(SESSION, 4000, Some(1_000.0)),
            listed(SESSION, 4000, Some(1_001.0)),
        ];
        assert_eq!(
            resolve_grok_session(&twice, 4000, &processes).unwrap(),
            SESSION
        );
        // A stale second entry does not create an ambiguity for the live one.
        let one_stale = [
            listed(SESSION, 4000, Some(1_000.0)),
            listed(OTHER_SESSION, 4000, Some(100.0)),
        ];
        let late = FakeProcesses::default().with(4000, GROK, Some(1_000.0));
        assert_eq!(
            resolve_grok_session(&one_stale, 4000, &late).unwrap(),
            SESSION
        );
        // Two Grok children of a wrapper with different sessions.
        let entries = [
            listed(SESSION, 4001, Some(1_000.0)),
            listed(OTHER_SESSION, 4002, Some(1_000.0)),
        ];
        let processes = FakeProcesses::default()
            .with(4000, "/bin/zsh", None)
            .with(4001, GROK, None)
            .with(4002, GROK, None)
            .child(4000, 4001)
            .child(4000, 4002);
        assert!(resolve_grok_session(&entries, 4000, &processes).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn real_processes_reject_a_stale_entry_whose_pid_is_not_grok() {
        // A live pid that is not Grok, listed as though it were: what Grok's
        // file holds after a session dies and its pid is reused.
        let mut sleeper = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = sleeper.id();
        let shell = Command::new("sh")
            .args(["-c", "sleep 30 & wait"])
            .spawn()
            .unwrap();
        let entries = [listed(SESSION, pid, Some(unix_now() as f64 + 5.0))];
        let facts = SystemProcesses.facts(pid).unwrap();
        assert!(facts.executable.as_deref().unwrap().ends_with("sleep"));
        let started = facts.started_unix.unwrap_or(unix_now() as f64);
        assert!((started - unix_now() as f64).abs() < 30.0, "{started}");
        let error = resolve_grok_session(&entries, pid, &SystemProcesses).unwrap_err();
        assert!(error.contains("not registered"), "{error}");
        // A dead pid is gone, not silently matched.
        let dead = sleeper.id();
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();
        assert!(SystemProcesses.facts(dead).is_none());
        assert!(
            resolve_grok_session(&entries, dead, &SystemProcesses)
                .unwrap_err()
                .contains("no longer running")
        );
        // Direct children come from the process table.
        // The shell forks its background sleep a moment after it starts.
        let waiting = Instant::now();
        let children = loop {
            let children = SystemProcesses.children(shell.id());
            if !children.is_empty() || waiting.elapsed() > Duration::from_secs(5) {
                break children;
            }
            thread::sleep(Duration::from_millis(20));
        };
        assert!(!children.is_empty(), "the shell's background sleep");
        assert!(SystemProcesses.children(u32::MAX - 1).is_empty());
        let mut shell = shell;
        for child in children {
            let _ = Command::new("kill").arg(child.to_string()).status();
        }
        let _ = shell.kill();
        let _ = shell.wait();
    }

    /// A temporary Grok home and a fake `grok` CLI that logs its arguments and
    /// prints the `response` file next to it.
    #[cfg(unix)]
    struct FakeGrok {
        root: PathBuf,
        home: PathBuf,
        executable: PathBuf,
    }

    #[cfg(unix)]
    impl FakeGrok {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let root = env::temp_dir().join(format!("riwork-grok-usage-{}", Uuid::new_v4()));
            let home = root.join("grok home");
            fs::create_dir_all(&home).unwrap();
            let bin = root.join("bin");
            fs::create_dir_all(&bin).unwrap();
            let executable = bin.join("grok");
            fs::write(
                &executable,
                "#!/bin/sh\nd=${0%/*}\nprintf '%s\\n' \"$*\" >> \"$d/calls\"\n\
                 [ -f \"$d/sleep\" ] && sleep \"$(cat \"$d/sleep\")\"\n\
                 [ -f \"$d/response\" ] && cat \"$d/response\"\n\
                 [ -f \"$d/fail\" ] && exit 3\nexit 0\n",
            )
            .unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                root,
                home,
                executable,
            }
        }

        fn respond(&self, body: &str) {
            fs::write(self.executable.with_file_name("response"), body).unwrap();
        }

        fn set(&self, name: &str, present: Option<&str>) {
            let path = self.executable.with_file_name(name);
            match present {
                Some(text) => fs::write(path, text).unwrap(),
                None => {
                    let _ = fs::remove_file(path);
                }
            }
        }

        fn list(&self, entries: Value) {
            fs::write(self.home.join("active_sessions.json"), entries.to_string()).unwrap();
        }

        fn calls(&self) -> Vec<String> {
            fs::read_to_string(self.executable.with_file_name("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn reader<'a>(
            &'a self,
            cache: &'a GrokUsageCache,
            processes: &'a FakeProcesses,
        ) -> GrokReader<'a> {
            GrokReader {
                home: Ok(self.home.clone()),
                executable: Ok(self.executable.clone()),
                timeout: Duration::from_secs(5),
                cache,
                processes,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for FakeGrok {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn rfc3339(unix: i64) -> String {
        chrono::DateTime::from_timestamp(unix, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
    }

    fn target(shell: &str, pid: u32) -> GrokTarget {
        GrokTarget {
            shell_id: shell.to_owned(),
            pane_pid: Ok(pid),
        }
    }

    #[cfg(unix)]
    #[test]
    fn grok_usage_is_read_for_the_session_listed_for_each_tab() {
        let grok = FakeGrok::new();
        grok.respond(&grok_report(SESSION).to_string());
        grok.list(json!([
            {"session_id": SESSION, "pid": 4000, "cwd": "/w", "opened_at": rfc3339(1_000)},
            // A stale entry: its pid is now the user's shell.
            {"session_id": OTHER_SESSION, "pid": 5000, "cwd": "/w", "opened_at": rfc3339(1_000)},
        ]));
        let processes = FakeProcesses::default().with(4000, GROK, Some(990.0)).with(
            5000,
            "/bin/zsh",
            Some(2_000.0),
        );
        let cache = GrokUsageCache::new(Duration::from_secs(60));
        let results = read_grok_usages_with(
            &grok.reader(&cache, &processes),
            &[target("live", 4000), target("stale", 5000)],
            &BTreeMap::new(),
            false,
        );
        let live = &results["live"];
        let usage = live.usage.as_ref().unwrap();
        assert_eq!(usage.session_id, SESSION);
        assert!(!live.stale && live.error.is_none());
        assert!(live.fetched_at_unix > 0 && live.checked_at_unix > 0);
        assert_eq!(live.pane_pid, Some(4000));
        // The stale entry yields no session and no spawn.
        assert!(results["stale"].usage.is_none());
        assert!(
            results["stale"]
                .error
                .as_ref()
                .unwrap()
                .contains("not registered")
        );
        assert_eq!(grok.calls(), [format!("usage {SESSION}")]);
    }

    #[cfg(unix)]
    #[test]
    fn grok_reads_are_cached_per_session_until_the_ttl_or_a_forced_refresh() {
        let grok = FakeGrok::new();
        grok.respond(&grok_report(SESSION).to_string());
        grok.list(json!([
            {"session_id": SESSION, "pid": 4000, "opened_at": rfc3339(1_000)},
            {"session_id": SESSION, "pid": 4100, "opened_at": rfc3339(1_000)},
            {"session_id": OTHER_SESSION, "pid": 4200, "opened_at": rfc3339(1_000)},
        ]));
        let processes = FakeProcesses::default()
            .with(4000, GROK, None)
            .with(4100, GROK, None)
            .with(4200, GROK, None);
        let cache = GrokUsageCache::new(Duration::from_millis(1500));
        let reader = grok.reader(&cache, &processes);
        let all = [target("a", 4000), target("b", 4100), target("c", 4200)];
        let none = BTreeMap::new();

        // Two tabs on one session share a read; the other session has its own.
        let first = read_grok_usages_with(&reader, &all, &none, false);
        assert_eq!(grok.calls().len(), 2, "{:?}", grok.calls());
        assert_eq!(first["a"].usage, first["b"].usage);
        // Every later tick inside the TTL is answered from the cache.
        for _ in 0..5 {
            read_grok_usages_with(&reader, &all, &first, false);
        }
        assert_eq!(grok.calls().len(), 2);
        // The Refresh action reads again.
        read_grok_usages_with(&reader, &all, &first, true);
        assert_eq!(grok.calls().len(), 4);
        // So does the first tick after the TTL.
        thread::sleep(Duration::from_millis(1600));
        read_grok_usages_with(&reader, &all, &first, false);
        assert_eq!(grok.calls().len(), 6);
        read_grok_usages_with(&reader, &all, &first, false);
        assert_eq!(grok.calls().len(), 6);
        // Only the session asked about is read.
        thread::sleep(Duration::from_millis(1600));
        read_grok_usages_with(&reader, &all[..1], &first, false);
        assert_eq!(grok.calls().len(), 7);
    }

    #[test]
    fn a_forced_read_that_waited_behind_another_is_not_repeated() {
        let cache = GrokUsageCache::new(Duration::from_secs(60));
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let usage = || {
            reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            thread::sleep(Duration::from_millis(400));
            parse_grok_usage(grok_report(SESSION).to_string().as_bytes(), SESSION)
        };
        thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| cache.get(SESSION, true, usage)))
                .collect();
            for handle in handles {
                assert!(handle.join().unwrap().usage.is_some());
            }
        });
        // Callers that asked while one read was running got its answer.
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_grok_read_keeps_the_last_figures_marked_stale() {
        let grok = FakeGrok::new();
        grok.respond(&grok_report(SESSION).to_string());
        grok.list(json!([{"session_id": SESSION, "pid": 4000, "opened_at": rfc3339(1_000)}]));
        let processes = FakeProcesses::default().with(4000, GROK, None);
        // A zero TTL makes every tick read again.
        let cache = GrokUsageCache::new(Duration::ZERO);
        let reader = grok.reader(&cache, &processes);
        let tabs = [target("a", 4000)];
        let good = read_grok_usages_with(&reader, &tabs, &BTreeMap::new(), false);
        assert!(!good["a"].stale);
        let figures = good["a"].usage.clone().unwrap();

        // Grok mid-write: the report is cut short.
        let report = grok_report(SESSION).to_string();
        grok.respond(&report[..report.len() / 3]);
        let partial = read_grok_usages_with(&reader, &tabs, &good, false);
        assert_eq!(partial["a"].usage.as_ref(), Some(&figures));
        assert!(partial["a"].stale);
        assert!(partial["a"].error.as_ref().unwrap().contains("unreadable"));
        assert_eq!(partial["a"].fetched_at_unix, good["a"].fetched_at_unix);

        // A non-zero exit, and no output at all, are the same story.
        grok.respond(&report);
        grok.set("fail", Some(""));
        let failed = read_grok_usages_with(&reader, &tabs, &partial, false);
        assert_eq!(failed["a"].usage.as_ref(), Some(&figures));
        assert!(failed["a"].stale);
        assert!(failed["a"].error.as_ref().unwrap().contains("exit 3"));
        grok.set("fail", None);
        grok.respond("");
        assert!(read_grok_usages_with(&reader, &tabs, &failed, false)["a"].stale);

        // Recovery clears the mark and updates the figures.
        grok.respond(&report.replace("\"turnCount\":10", "\"turnCount\":11"));
        let healed = read_grok_usages_with(&reader, &tabs, &failed, false);
        assert!(!healed["a"].stale && healed["a"].error.is_none());
        assert_eq!(healed["a"].usage.as_ref().unwrap().turns, 11);

        // A tab that never read successfully has no figures to keep.
        let fresh_cache = GrokUsageCache::new(Duration::ZERO);
        grok.respond("{");
        let never = read_grok_usages_with(
            &grok.reader(&fresh_cache, &processes),
            &tabs,
            &BTreeMap::new(),
            false,
        );
        assert!(never["a"].usage.is_none() && !never["a"].stale);
        assert!(never["a"].error.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_or_half_written_active_session_list_is_handled() {
        let grok = FakeGrok::new();
        grok.respond(&grok_report(SESSION).to_string());
        let processes = FakeProcesses::default().with(4000, GROK, None);
        let cache = GrokUsageCache::new(Duration::ZERO);
        let reader = grok.reader(&cache, &processes);
        let tabs = [target("a", 4000)];

        // Grok has never written the file.
        let none = read_grok_usages_with(&reader, &tabs, &BTreeMap::new(), false);
        assert!(none["a"].error.as_ref().unwrap().contains("not registered"));

        grok.list(json!([{"session_id": SESSION, "pid": 4000, "opened_at": rfc3339(1_000)}]));
        let good = read_grok_usages_with(&reader, &tabs, &BTreeMap::new(), false);
        assert!(good["a"].usage.is_some());

        // The leader is rewriting it: the last figures stay, marked stale.
        for garbage in ["", "[{\"session_id\": \"01a0", "not json"] {
            fs::write(grok.home.join("active_sessions.json"), garbage).unwrap();
            let kept = read_grok_usages_with(&reader, &tabs, &good, false);
            assert_eq!(kept["a"].usage, good["a"].usage, "{garbage:?}");
            assert!(kept["a"].stale, "{garbage:?}");
            assert!(
                kept["a"]
                    .error
                    .as_ref()
                    .unwrap()
                    .contains("active sessions")
            );
            // Nothing was spawned to answer it.
        }
        assert_eq!(grok.calls().len(), 1);
        // The tab's process changed while the list was unreadable: the old
        // figures belong to a different Grok and are dropped.
        let processes = FakeProcesses::default().with(4001, GROK, None);
        let moved = read_grok_usages_with(
            &grok.reader(&cache, &processes),
            &[target("a", 4001)],
            &good,
            false,
        );
        assert!(moved["a"].usage.is_none());
        assert!(moved["a"].error.is_some());
        // tmux failing to name the pane process is the same kind of gap.
        let unknown = GrokTarget {
            shell_id: "a".into(),
            pane_pid: Err("tmux is not running".into()),
        };
        let kept = read_grok_usages_with(&reader, &[unknown], &good, false);
        assert!(kept["a"].stale && kept["a"].usage.is_some());
        assert_eq!(kept["a"].error.as_deref(), Some("tmux is not running"));
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_grok_cli_is_reported_per_tab() {
        let grok = FakeGrok::new();
        grok.list(json!([{"session_id": SESSION, "pid": 4000, "opened_at": rfc3339(1_000)}]));
        let processes = FakeProcesses::default().with(4000, GROK, None);
        let cache = GrokUsageCache::new(Duration::ZERO);
        let mut reader = grok.reader(&cache, &processes);
        reader.executable = Err("Grok is not installed or is not on PATH".into());
        let results = read_grok_usages_with(&reader, &[target("a", 4000)], &BTreeMap::new(), false);
        assert_eq!(
            results["a"].error.as_deref(),
            Some("Grok is not installed or is not on PATH")
        );
        assert!(read_grok_usages_with(&reader, &[], &BTreeMap::new(), false).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn grok_usage_reads_time_out_and_kill_the_whole_process_group() {
        let grok = FakeGrok::new();
        grok.respond(&grok_report(SESSION).to_string());
        // The fake sleeps as a child of its script: both must go.
        grok.set("sleep", Some("30"));
        let started = Instant::now();
        let error =
            run_grok_usage(&grok.executable, SESSION, Duration::from_millis(200)).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );

        // Through the reader the timeout is that tab's error, promptly.
        grok.list(json!([{"session_id": SESSION, "pid": 4000, "opened_at": rfc3339(1_000)}]));
        let processes = FakeProcesses::default().with(4000, GROK, None);
        let cache = GrokUsageCache::new(Duration::from_secs(60));
        let mut reader = grok.reader(&cache, &processes);
        reader.timeout = Duration::from_millis(200);
        let started = Instant::now();
        let results = read_grok_usages_with(&reader, &[target("a", 4000)], &BTreeMap::new(), false);
        assert!(results["a"].error.as_ref().unwrap().contains("timed out"));
        assert!(results["a"].usage.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );

        // And a prompt answer is unaffected by the limit.
        grok.set("sleep", None);
        assert!(run_grok_usage(&grok.executable, SESSION, Duration::from_secs(5)).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn grok_output_is_bounded() {
        let started = Instant::now();
        let mut yes = Command::new("yes");
        yes.stdin(Stdio::null()).stderr(Stdio::null());
        let error = run_bounded(yes, Duration::from_secs(5), 64 * 1024).unwrap_err();
        assert!(error.contains("too large"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        let mut echo = Command::new("echo");
        echo.arg("ok").stdin(Stdio::null());
        let (status, output) = run_bounded(echo, Duration::from_secs(5), 16).unwrap();
        assert!(status.success());
        assert_eq!(output, b"ok\n");
        let missing = Command::new("/nonexistent/riwork-grok");
        assert!(run_bounded(missing, Duration::from_secs(1), 16).is_err());
    }

    #[test]
    fn a_session_with_nothing_counted_yet_is_unavailable_not_free() {
        let zero = json!({"sessionId": SESSION, "session": {
            "inputTokens": 0, "outputTokens": 0, "cachedReadTokens": 0,
            "cacheCreationTokens": 0, "reasoningTokens": 0, "totalTokens": 0,
            "modelCalls": 0, "costUsdTicks": 0, "turnCount": 0, "modelUsage": {}
        }});
        let error = parse_grok_usage(zero.to_string().as_bytes(), SESSION).unwrap_err();
        assert!(error.contains("no usage recorded"), "{error}");
        // One counted turn is real usage, even at zero cost.
        let turn = json!({"sessionId": SESSION, "session": {"totalTokens": 0, "turnCount": 1}});
        assert!(parse_grok_usage(turn.to_string().as_bytes(), SESSION).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_session_that_never_read_is_not_retried_faster_than_the_ttl() {
        let grok = FakeGrok::new();
        grok.set("fail", Some(""));
        let cache = GrokUsageCache::new(Duration::from_secs(60));
        for _ in 0..4 {
            let read = cache.get(SESSION, false, || {
                run_grok_usage(&grok.executable, SESSION, Duration::from_secs(5))
            });
            assert!(read.usage.is_none() && read.error.is_some());
        }
        assert_eq!(grok.calls().len(), 1);
        // After good figures, a failure is retried on the shorter schedule.
        grok.set("fail", None);
        grok.respond(&grok_report(OTHER_SESSION).to_string());
        let quick = GrokUsageCache::new(Duration::from_secs(600));
        let read = |cache: &GrokUsageCache| {
            cache.get(OTHER_SESSION, false, || {
                run_grok_usage(&grok.executable, OTHER_SESSION, Duration::from_secs(5))
            })
        };
        assert!(read(&quick).usage.is_some());
        assert_eq!(grok.calls().len(), 2);
        {
            let slot = quick.sessions.lock().unwrap()[OTHER_SESSION].clone();
            let mut entry = slot.lock().unwrap();
            entry.read.error = Some("mid-write".into());
            entry.attempted =
                Some(Instant::now() - GROK_RETRY_AFTER_FAILURE - Duration::from_secs(1));
        }
        assert!(read(&quick).error.is_none());
        assert_eq!(grok.calls().len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn a_descendant_holding_the_output_pipe_does_not_hold_the_read() {
        // The child exits at once; something it started keeps stdout open.
        let root = env::temp_dir().join(format!("riwork-grok-straggler-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let pid_file = root.join("straggler.pid");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(format!(
                "echo ok; sleep 30 & echo $! > '{}'",
                pid_file.display()
            ))
            .stdin(Stdio::null());
        let started = Instant::now();
        let (status, output) = run_bounded(command, Duration::from_secs(5), 1024).unwrap();
        // The straggler is in the child's group, so a real timeout would kill
        // it; a normal exit leaves it. Either way this call returned promptly.
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
        assert!(status.success());
        assert_eq!(output, b"ok\n");
        if let Ok(pid) = fs::read_to_string(&pid_file) {
            let _ = Command::new("kill").arg(pid.trim()).status();
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn grok_is_asked_for_exactly_its_documented_report_and_nothing_else() {
        let grok = FakeGrok::new();
        grok.respond(&grok_report(SESSION).to_string());
        run_grok_usage(&grok.executable, SESSION, Duration::from_secs(5)).unwrap();
        // No turn, no debug flags, no leader socket; the id is the only input.
        assert_eq!(grok.calls(), [format!("usage {SESSION}")]);
    }

    #[test]
    fn grok_provider_json_keeps_windows_empty_and_only_adds_fields() {
        let usage = parse_grok_usage(grok_report(SESSION).to_string().as_bytes(), SESSION).unwrap();
        let report = grok_provider_usage(&tab(Some(usage), false, None));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["provider"], "grok");
        assert_eq!(json["windows"], json!([]));
        assert_eq!(json["account_label"], "unknown");
        assert!(json["context_used_percent"].is_null());
        assert!(json.get("session_error").is_none());
        assert!((json["session_cost_usd"].as_f64().unwrap() - 26.61771192).abs() < 1e-9);
        let session = &json["session"];
        assert_eq!(session["session_id"], SESSION);
        assert_eq!(session["primary_model"], "grok-4.7-build-fast");
        assert_eq!(session["turns"], 10);
        assert_eq!(session["model_calls"], 470);
        assert_eq!(session["updated_at"], "2026-09-29T13:02:35Z");
        assert_eq!(session["tokens"]["input"], 60_210_005u64);
        assert_eq!(session["tokens"]["output"], 491_382u64);
        assert_eq!(session["tokens"]["cached_read"], 56_722_048u64);
        assert_eq!(session["tokens"]["cache_creation"], 12);
        assert_eq!(session["tokens"]["reasoning"], 346_297u64);
        assert_eq!(session["tokens"]["total"], 60_701_387u64);
        assert_eq!(session["models"].as_array().unwrap().len(), 2);
        assert_eq!(session["models"][0]["tokens"]["total"], 60_400_000u64);
        // The existing consumers' fields are all still there.
        let restored: ProviderUsage = serde_json::from_value(json).unwrap();
        assert_eq!(restored.provider, "grok");
        assert!(restored.session.is_some());

        let unavailable = serde_json::to_value(grok_provider_usage(&tab(
            None,
            false,
            Some("Grok has not registered a session for this tab yet"),
        )))
        .unwrap();
        assert_eq!(unavailable["windows"], json!([]));
        assert_eq!(unavailable["account_label"], "unknown");
        assert!(unavailable.get("session").is_none());
        assert_eq!(
            unavailable["session_error"],
            "Grok has not registered a session for this tab yet"
        );
    }

    #[test]
    fn other_providers_json_gains_no_grok_fields() {
        let claude = parse_claude_usage(&json!({"cost": {"total_cost_usd": 0.5}}), 1).unwrap();
        let json = serde_json::to_value(&claude).unwrap();
        assert!(json.get("session").is_none());
        assert!(json.get("session_error").is_none());
        let codex = parse_codex_usage(
            &json!({"rateLimits": {"primary": {"usedPercent": 1, "windowDurationMins": 60}}}),
            None,
            1,
        )
        .unwrap();
        let json = serde_json::to_value(&codex).unwrap();
        assert!(json.get("session").is_none() && json.get("session_error").is_none());
        // A cache file written before these fields existed still reads back.
        let old: ProviderUsage = serde_json::from_str(
            r#"{"provider":"claude","windows":[],"updated_at_unix":1,"account_label":null}"#,
        )
        .unwrap();
        assert!(old.session.is_none() && old.session_error.is_none());
    }

    #[test]
    fn token_counts_and_costs_format_compactly() {
        for (count, text) in [
            (0, "0"),
            (999, "999"),
            (1_000, "1k"),
            (1_234, "1.2k"),
            (12_500, "12.5k"),
            (99_949, "99.9k"),
            (128_000, "128k"),
            (999_499, "999k"),
            (999_500, "1M"),
            (1_000_000, "1M"),
            (60_701_387, "60.7M"),
            (128_000_000, "128M"),
            (1_500_000_000, "1.5B"),
            (u64::MAX, "18446744074B"),
        ] {
            assert_eq!(format_tokens(count), text, "{count}");
        }
        assert_eq!(format_usd(0.42), "$0.42");
        assert_eq!(format_usd(0.0), "$0.00");
        assert_eq!(format_usd(0.001), "<$0.01");
        assert_eq!(format_usd(26.61771192), "$26.62");
        assert_eq!(format_usd(1234.5), "$1234.50");
    }

    #[test]
    fn the_chip_shows_session_cost_and_tokens_for_grok() {
        let usage = SessionUsage {
            session_id: SESSION.into(),
            updated_at: None,
            updated_at_unix: None,
            primary_model: None,
            turns: 3,
            model_calls: 9,
            tokens: TokenCounts {
                total: 128_000,
                ..TokenCounts::default()
            },
            cost_usd: Some(0.42),
            models: Vec::new(),
        };
        assert_eq!(
            grok_chip_label(Some(&tab(Some(usage.clone()), false, None))),
            "GROK $0.42 · 128k tok"
        );
        assert_eq!(
            grok_chip_label(Some(&tab(Some(usage.clone()), true, Some("late")))),
            "GROK* $0.42 · 128k tok"
        );
        let free = SessionUsage {
            cost_usd: None,
            ..usage
        };
        assert_eq!(
            grok_chip_label(Some(&tab(Some(free), false, None))),
            "GROK · 128k tok"
        );
        assert_eq!(grok_chip_label(None), "GROK · LOADING");
        assert_eq!(
            grok_chip_label(Some(&tab(None, false, Some("nothing yet")))),
            "GROK · —"
        );
    }

    #[test]
    fn combined_grok_totals_count_each_session_once() {
        let session = |id: &str, cost: Option<f64>, total: u64| SessionUsage {
            session_id: id.into(),
            updated_at: None,
            updated_at_unix: None,
            primary_model: None,
            turns: 1,
            model_calls: 1,
            tokens: TokenCounts {
                input: total / 2,
                output: total / 2,
                total,
                ..TokenCounts::default()
            },
            cost_usd: cost,
            models: Vec::new(),
        };
        let a = session("a", Some(1.25), 1_000);
        let b = session("b", None, 500);
        let totals = grok_totals([&a, &b, &a]);
        assert_eq!(totals.sessions, 2);
        assert_eq!(totals.cost_usd, 1.25);
        assert_eq!(totals.tokens.total, 1_500);
        assert_eq!(totals.tokens.input, 750);
        assert_eq!(grok_totals([]), GrokTotals::default());
    }

    #[test]
    fn a_tab_without_figures_is_asked_again_sooner_than_one_with() {
        let with = tab(
            Some(parse_grok_usage(grok_report(SESSION).to_string().as_bytes(), SESSION).unwrap()),
            false,
            None,
        );
        assert!(!with.is_due(100 + GROK_USAGE_REFRESH.as_secs() - 1));
        assert!(with.is_due(100 + GROK_USAGE_REFRESH.as_secs()));
        let without = tab(None, false, Some("not yet"));
        assert!(!without.is_due(102));
        assert!(without.is_due(105));
        // The cache expires before a window asks again, so its tick reads.
        assert!(GROK_CACHE_TTL < GROK_USAGE_REFRESH);
    }
}
