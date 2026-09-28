//! Usage supplied by the official harnesses, without reading credentials or
//! replaying terminal input. Codex reads run on a worker; Claude pushes its
//! documented status-line payload into a small per-shell cache.

use std::{
    collections::BTreeSet,
    env, fs,
    fs::OpenOptions,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const CODEX_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_SERVER_OUTPUT: u64 = 2 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub provider: String,
    pub windows: Vec<UsageWindow>,
    pub updated_at_unix: u64,
    pub account_label: Option<String>,
    #[serde(default)]
    pub context_used_percent: Option<f64>,
    #[serde(default)]
    pub session_cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
}
