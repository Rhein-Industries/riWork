//! Invocation-scoped Codex/Grok hooks. No user hook files are rewritten, and
//! only identifiers and lifecycle states survive the stdin envelope.
use super::{MAX_CURSOR_BYTES, MAX_INPUT_BYTES, TURN_QUIET_SECS, private_file, valid_identifier};
use crate::{
    activity::{AgentActivity, AgentState, unix_now},
    notifications::CompletionOutcome,
    sessions::{HarnessKind, SessionManager, ShellSession},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
use uuid::Uuid;

const CODEX_EVENTS: [(&str, &str); 7] = [
    ("SessionStart", "session_start"),
    ("UserPromptSubmit", "user_prompt_submit"),
    ("PermissionRequest", "permission_request"),
    ("PostToolUse", "post_tool_use"),
    ("Stop", "stop"),
    ("Interrupt", "interrupt"),
    ("PreToolUse", "pre_tool_use"),
];

fn command(executable: &Path, home: &Path, shell: &str, provider: &str) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    format!(
        "{} agent-hook {} {} {}",
        quote(&executable.to_string_lossy()),
        provider,
        quote(&home.to_string_lossy()),
        quote(shell)
    )
}

pub(crate) fn codex_arguments(executable: &Path, home: &Path, shell: &str) -> Vec<String> {
    if Uuid::parse_str(shell).is_err() {
        return Vec::new();
    }
    let command = command(executable, home, shell, "codex");
    let mut hooks = serde_json::Map::new();
    let mut state = serde_json::Map::new();
    for (event, key) in CODEX_EVENTS {
        let timeout = if event == "Interrupt" { 3 } else { 10 };
        let handler =
            json!({"type":"command", "command":command, "timeout":timeout, "async":false});
        hooks.insert(event.into(), json!([{"hooks":[handler.clone()]}]));
        // Codex 0.162's normalized hook identity (hooks/engine/discovery.rs).
        // Trust exactly our handler in the session-flags layer. Never bypass
        // trust for user/project/plugin hooks, and never persist an approval.
        // TOML omits None fields; the remaining JSON is recursively sorted.
        let hash = trust_hash(key, handler);
        state.insert(
            format!("/<session-flags>/config.toml:{key}:0:0"),
            json!({"trusted_hash":hash}),
        );
    }
    hooks.insert("state".into(), Value::Object(state));
    vec![
        "-c".into(),
        format!("hooks={}", to_toml(&Value::Object(hooks))),
    ]
}

fn trust_hash(event: &str, handler: Value) -> String {
    // version_for_toml in Codex 0.162.1 converts normalized TOML to sorted
    // JSON, serializes compactly, then SHA-256 hashes those UTF-8 bytes.
    let mut identity = json!({"event_name":event, "hooks":[handler]});
    identity.sort_all_objects();
    format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&identity).unwrap())
    )
}

// Inline TOML is also accepted by Codex's -c parser. JSON string escaping is
// valid for these TOML basic strings; JSON objects themselves are not TOML.
fn to_toml(value: &Value) -> String {
    match value {
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}={}", json!(k), to_toml(v)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(to_toml).collect::<Vec<_>>().join(",")
        ),
        _ => value.to_string(),
    }
}

pub(crate) fn grok_hooks(executable: &Path, home: &Path, shell: &str) -> Value {
    let command = command(executable, home, shell, "grok");
    [
        "SessionStart",
        "SessionEnd",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "Stop",
        "StopCancelled",
        "StopFailure",
        "Notification",
    ]
    .into_iter()
    .map(|event| {
        let mut group = json!({"hooks":[{"type":"command", "command":command, "timeout":10}]});
        if event == "Notification" {
            group["matcher"] = json!("^(permission_prompt|idle_prompt)$");
        }
        (event.into(), json!([group]))
    })
    .collect::<serde_json::Map<_, _>>()
    .into()
}

#[derive(Deserialize)]
struct Input {
    #[serde(alias = "sessionId")]
    session_id: String,
    hook_event_name: String,
    #[serde(default, alias = "promptId", alias = "turn_id")]
    prompt_id: Option<String>,
    #[serde(default, alias = "notificationType")]
    notification_type: Option<String>,
    #[serde(default, alias = "subagentType", alias = "agentId")]
    agent_id: Option<Value>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
struct Cursor {
    session: String,
    turn: String,
    phase: String,
    since: u64,
    #[serde(default)]
    seen: u64,
    #[serde(default)]
    outcome: CompletionOutcome,
}

pub fn record_harness_hook(
    home: &Path,
    shell: &str,
    provider: HarnessKind,
    body: &str,
) -> Result<(), String> {
    if !matches!(provider, HarnessKind::Codex | HarnessKind::Grok) || body.len() > MAX_INPUT_BYTES {
        return Err("Invalid lifecycle hook".into());
    }
    let mut envelope: Value = serde_json::from_str(body).map_err(|_| "Invalid lifecycle input")?;
    // Grok sends both its camelCase keys and Claude-compatible snake_case
    // aliases. Serde aliases alone reject that envelope as a duplicate field.
    if let Some(map) = envelope.as_object_mut() {
        for (snake, aliases) in [
            ("session_id", &["sessionId"][..]),
            ("prompt_id", &["promptId", "turn_id"][..]),
            ("notification_type", &["notificationType"][..]),
            ("agent_id", &["agentId", "subagentType"][..]),
        ] {
            for alias in aliases {
                if let Some(value) = map.remove(*alias) {
                    map.entry(snake).or_insert(value);
                }
            }
        }
    }
    let input: Input = serde_json::from_value(envelope).map_err(|_| "Invalid lifecycle input")?;
    if !valid_identifier(&input.session_id) || input.agent_id.is_some() {
        return Ok(());
    }
    let session = SessionManager::at(home.into())?.registered_session(shell)?;
    if session.harness != Some(provider) {
        return Ok(());
    }
    if provider == HarnessKind::Codex
        && matches!(
            input.hook_event_name.as_str(),
            "SessionStart" | "UserPromptSubmit"
        )
        && let Some(log_home) = crate::activity::resolved_codex_home(home, &session)
    {
        crate::activity::bind_codex_thread(home, shell, &input.session_id, &log_home)?;
    }
    let directory = home.join("agent-hooks").join(provider.program());
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let lock = private_file(&directory.join(format!("{shell}.lock")), false)?;
    lock.lock_exclusive().map_err(|e| e.to_string())?;
    let path = directory.join(format!("{shell}.json"));
    let previous = read_cursor(&path);
    if previous
        .as_ref()
        .is_some_and(|cursor| cursor.session != input.session_id)
        && !matches!(
            input.hook_event_name.as_str(),
            "SessionStart" | "UserPromptSubmit"
        )
    {
        return Ok(());
    }
    if input.hook_event_name == "SessionStart"
        && input.source.as_deref() == Some("compact")
        && previous.is_some()
    {
        return Ok(());
    }
    let mut cursor = previous
        .clone()
        .filter(|c| c.session == input.session_id)
        .unwrap_or(Cursor {
            session: input.session_id,
            turn: String::new(),
            phase: "waiting".into(),
            since: unix_now(),
            seen: unix_now(),
            outcome: CompletionOutcome::Completed,
        });
    let phase = match input.hook_event_name.as_str() {
        "SessionStart" => "waiting",
        "UserPromptSubmit" => {
            cursor.turn = input
                .prompt_id
                .clone()
                .filter(|s| valid_identifier(s))
                .unwrap_or_default();
            cursor.outcome = CompletionOutcome::Completed;
            "working"
        }
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => "working",
        "PermissionRequest" => "waiting",
        "Notification" if input.notification_type.as_deref() == Some("permission_prompt") => {
            "waiting"
        }
        "Notification" if input.notification_type.as_deref() == Some("idle_prompt") => "done",
        "SessionEnd" => "done",
        "StopFailure" => {
            cursor.outcome = CompletionOutcome::Failed;
            "done"
        }
        "Interrupt" | "StopCancelled" => {
            cursor.outcome = CompletionOutcome::Interrupted;
            "done"
        }
        "Stop"
            if input.prompt_id.is_none()
                || provider == HarnessKind::Codex
                || matches!(
                    input.reason.as_deref(),
                    Some("end_turn" | "channel_closed" | "shutdown")
                ) =>
        {
            "done"
        }
        _ => return Ok(()),
    };
    // Tool/stop events from an old turn cannot overwrite the current one.
    if input.hook_event_name != "UserPromptSubmit"
        && input
            .prompt_id
            .as_ref()
            .is_some_and(|id| !cursor.turn.is_empty() && id != &cursor.turn)
    {
        return Ok(());
    }
    if phase != cursor.phase {
        cursor.since = unix_now();
    }
    cursor.phase = phase.into();
    cursor.seen = unix_now();
    // An unscoped end/idle notification settles the display, but is not proof
    // that the remembered prompt finished. Never attribute it to that turn.
    if phase == "done"
        && (input.prompt_id.is_none() || input.notification_type.as_deref() == Some("idle_prompt"))
    {
        cursor.turn.clear();
    }
    if phase == "done" {
        complete(home, shell, &session, &cursor)?;
    }
    // A fast tool's PreToolUse and PostToolUse come within the same second and
    // leave the cursor as it was: the file already says this, so it is not
    // written and synced again while the agent waits on its hook.
    if previous.as_ref() == Some(&cursor) {
        return Ok(());
    }
    save_cursor(&path, &cursor)
}

fn complete(
    home: &Path,
    shell: &str,
    session: &ShellSession,
    cursor: &Cursor,
) -> Result<(), String> {
    if cursor.turn.is_empty() {
        return Ok(());
    }
    let provider = session.harness.ok_or("Missing harness")?;
    let event = if provider == HarnessKind::Codex {
        let log_home = crate::activity::resolved_codex_home(home, session)
            .ok_or("Codex account home is unknown")?;
        crate::activity::codex_completion_event_id(&log_home, &cursor.session, &cursor.turn)
    } else {
        json!([provider.program(), cursor.session, cursor.turn]).to_string()
    };
    crate::notifications::record_completion_outcome(home, shell, &event, provider, cursor.outcome)?;
    Ok(())
}

fn save_cursor(path: &Path, cursor: &Cursor) -> Result<(), String> {
    let directory = path.parent().ok_or("Invalid lifecycle cursor path")?;
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = private_file(&temporary, true)?;
        serde_json::to_writer(&mut file, cursor).map_err(|e| e.to_string())?;
        file.write_all(b"\n")
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temporary, &path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn read_cursor(path: &Path) -> Option<Cursor> {
    let file = fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_CURSOR_BYTES {
        return None;
    }
    serde_json::from_reader(file.take(MAX_CURSOR_BYTES + 1)).ok()
}

pub fn harness_state(home: &Path, shell: &str, provider: HarnessKind) -> Option<AgentState> {
    harness_snapshot(home, shell, provider, unix_now(), true).map(|(state, _)| state)
}

pub(crate) fn harness_snapshot(
    home: &Path,
    shell: &str,
    provider: HarnessKind,
    now: u64,
    quiet: bool,
) -> Option<(AgentState, u64)> {
    let directory = home.join("agent-hooks").join(provider.program());
    let path = directory.join(format!("{shell}.json"));
    let lock = private_file(&directory.join(format!("{shell}.lock")), false).ok()?;
    lock.lock_exclusive().ok()?;
    let mut cursor = read_cursor(&path)?;
    let observed = cursor.seen.max(cursor.since);
    if quiet && cursor.phase == "working" && now.saturating_sub(observed) > TURN_QUIET_SECS {
        cursor.phase = "done".into();
        cursor.since = observed.saturating_add(TURN_QUIET_SECS + 1);
        cursor.outcome = CompletionOutcome::Interrupted;
        let session = SessionManager::at(home.into())
            .ok()?
            .registered_session(shell)
            .ok()?;
        complete(home, shell, &session, &cursor).ok()?;
        save_cursor(&path, &cursor).ok()?;
    }
    let activity = match cursor.phase.as_str() {
        "working" => AgentActivity::Working,
        "done" => AgentActivity::Done,
        "waiting" => AgentActivity::Waiting,
        _ => return None,
    };
    Some((
        AgentState {
            since_unix: Some(cursor.since),
            ..AgentState::plain(activity)
        },
        observed,
    ))
}

pub(crate) fn settle_rollout(
    home: &Path,
    shell: &ShellSession,
    thread: &str,
    turn: &str,
    since: u64,
    outcome: CompletionOutcome,
) -> Result<(), String> {
    let directory = home.join("agent-hooks/codex");
    let path = directory.join(format!("{}.json", shell.id));
    let lock = private_file(&directory.join(format!("{}.lock", shell.id)), false)?;
    lock.lock_exclusive().map_err(|e| e.to_string())?;
    let Some(mut cursor) = read_cursor(&path) else {
        return Ok(());
    };
    if cursor.session != thread || cursor.turn != turn || cursor.seen.max(cursor.since) > since {
        return Ok(());
    }
    cursor.phase = "done".into();
    cursor.since = since;
    cursor.seen = since;
    cursor.outcome = outcome;
    complete(home, &shell.id, shell, &cursor)?;
    save_cursor(&path, &cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_hook_that_changes_nothing_leaves_the_cursor_file_alone() {
        use std::os::unix::fs::MetadataExt;
        let home = std::env::temp_dir().join(format!("riwork-hook-unchanged-{}", Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        let shell = Uuid::new_v4().to_string();
        fs::write(
            home.join("sessions.json"),
            json!({"sessions":[{"id":shell,"kind":"project","cwd":"/tmp","harness":"codex","created_at_unix":1}]})
                .to_string(),
        )
        .unwrap();
        let record = |event: &str| {
            let body = json!({"session_id":"00000000-0000-4000-8000-000000000002","hook_event_name":event,"prompt_id":"turn-a"});
            record_harness_hook(&home, &shell, HarnessKind::Codex, &body.to_string()).unwrap();
        };
        let path = home.join(format!("agent-hooks/codex/{shell}.json"));
        let written = || {
            let cursor = read_cursor(&path).unwrap();
            (
                fs::metadata(&path).unwrap().ino(),
                cursor.phase,
                cursor.seen,
            )
        };
        record("UserPromptSubmit");
        // Every write replaces the file, so an unchanged inode means no write.
        // A pair that straddles a second moves `seen` and is written; try again.
        let mut skipped = false;
        for _ in 0..5 {
            record("PreToolUse");
            let before = written();
            record("PostToolUse");
            let after = written();
            assert_eq!(after.1, "working");
            if after.2 == before.2 {
                assert_eq!(after.0, before.0, "an unchanged cursor was written again");
                skipped = true;
                break;
            }
            assert_ne!(after.0, before.0, "a newer `seen` was not written");
        }
        assert!(skipped);
        // A change is written at once.
        let before = written();
        record("PermissionRequest");
        let after = written();
        assert_ne!(after.0, before.0);
        assert_eq!(after.1, "waiting");
        assert_eq!(
            harness_state(&home, &shell, HarnessKind::Codex)
                .unwrap()
                .activity,
            AgentActivity::Waiting
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    #[ignore = "timing: cargo test -p riwork-engine --lib -- --ignored --nocapture time_codex_tool_hooks"]
    fn time_codex_tool_hooks_with_a_long_registry() {
        let home = std::env::temp_dir().join(format!("riwork-hook-timing-{}", Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        if SessionManager::at(home.clone()).is_err() {
            return;
        }
        // A long-used registry: 174 shells, about 370 KB.
        let shell = Uuid::new_v4().to_string();
        let rows = std::iter::once(json!({"id":shell,"kind":"project","cwd":"/tmp","harness":"codex","created_at_unix":1}))
            .chain((0..173).map(|_| json!({"id":Uuid::new_v4().to_string(),"kind":"project","cwd":"/tmp","harness":"codex","command":"x".repeat(2100),"created_at_unix":1})))
            .collect::<Vec<_>>();
        fs::write(
            home.join("sessions.json"),
            json!({ "sessions": rows }).to_string(),
        )
        .unwrap();
        let record = |event: &str| {
            let body = json!({"session_id":"00000000-0000-4000-8000-000000000002","hook_event_name":event,"prompt_id":"turn-a"});
            record_harness_hook(&home, &shell, HarnessKind::Codex, &body.to_string()).unwrap();
        };
        record("UserPromptSubmit");
        let rounds = 50;
        // A fast tool: its PreToolUse and PostToolUse come within the same second.
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            record("PreToolUse");
            record("PostToolUse");
        }
        eprintln!(
            "Codex tool hook: {:?} per call",
            started.elapsed() / (2 * rounds)
        );
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            SessionManager::at(home.clone())
                .unwrap()
                .registered_session(&shell)
                .unwrap();
        }
        eprintln!(
            "  registry lookup: {:?} per call",
            started.elapsed() / rounds
        );
        let path = home.join(format!("agent-hooks/codex/{shell}.json"));
        let cursor = read_cursor(&path).unwrap();
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            save_cursor(&path, &cursor).unwrap();
        }
        eprintln!("  cursor save: {:?} per call", started.elapsed() / rounds);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn real_envelopes_track_approvals_and_queue_each_worker_completion_once() {
        for provider in [HarnessKind::Codex, HarnessKind::Grok] {
            let home = std::env::temp_dir().join(format!("riwork-lifecycle-{}", Uuid::new_v4()));
            fs::create_dir_all(home.join("project")).unwrap();
            let store = crate::store::Store::open(home.clone()).unwrap();
            let project = store
                .add_project(home.join("project"), Some("Hooks"))
                .unwrap();
            store.set_project_notifications(&project.id, true).unwrap();
            store
                .set_project_orchestrator_wakes(&project.id, true)
                .unwrap();
            let shell = Uuid::new_v4().to_string();
            fs::write(
                home.join("sessions.json"),
                json!({"sessions":[{
                    "id":shell,"project_id":project.id,"kind":"project","cwd":project.root,
                    "harness":provider.program(),"created_at_unix":1,"user_opened":false
                }]})
                .to_string(),
            )
            .unwrap();
            let record = |event: &str, turn: &str, extra: Value| {
                let mut body = json!({"session_id":"00000000-0000-4000-8000-000000000002", "hook_event_name":event,
                    "prompt_id":turn,"prompt":"PRIVATE PROMPT", "last_assistant_message":"PRIVATE RESPONSE"});
                if provider == HarnessKind::Grok {
                    body["sessionId"] = json!("00000000-0000-4000-8000-000000000002");
                    body["promptId"] = json!(turn);
                }
                body.as_object_mut()
                    .unwrap()
                    .extend(extra.as_object().unwrap().clone());
                record_harness_hook(&home, &shell, provider, &body.to_string()).unwrap();
            };
            let state = || harness_state(&home, &shell, provider).unwrap().activity;
            record("UserPromptSubmit", "turn-a", json!({}));
            assert_eq!(state(), AgentActivity::Working);
            record("SessionStart", "turn-a", json!({"source":"compact"}));
            assert_eq!(state(), AgentActivity::Working);
            if provider == HarnessKind::Codex {
                record("PermissionRequest", "turn-a", json!({}));
            } else {
                record(
                    "Notification",
                    "turn-a",
                    json!({"notificationType":"permission_prompt"}),
                );
            }
            assert_eq!(state(), AgentActivity::Waiting);
            record("PostToolUse", "turn-a", json!({}));
            assert_eq!(state(), AgentActivity::Working);
            record("Stop", "old-turn", json!({"reason":"end_turn"}));
            record(
                "Stop",
                "turn-a",
                json!({"agent_id":"child","reason":"end_turn"}),
            );
            assert_eq!(state(), AgentActivity::Working);
            record("Stop", "turn-a", json!({"reason":"end_turn"}));
            record("Stop", "turn-a", json!({"reason":"end_turn"}));
            assert_eq!(state(), AgentActivity::Done);
            let wakes: Value =
                serde_json::from_slice(&fs::read(home.join("orchestrator-wakes.json")).unwrap())
                    .unwrap();
            assert_eq!(
                wakes["entries"].as_array().unwrap().len(),
                2,
                "one wake per scope, no replay"
            );
            record("UserPromptSubmit", "turn-b", json!({}));
            record(
                if provider == HarnessKind::Codex {
                    "Interrupt"
                } else {
                    "StopCancelled"
                },
                "turn-b",
                json!({}),
            );
            assert_eq!(state(), AgentActivity::Done);
            let cursor = fs::read_to_string(
                home.join("agent-hooks")
                    .join(provider.program())
                    .join(format!("{shell}.json")),
            )
            .unwrap();
            assert!(!cursor.contains("PRIVATE"));
            let sampled = crate::activity::states_once(
                &home,
                &[serde_json::from_value(json!({
                    "id":shell,"project_id":project.id,"kind":"project","cwd":project.root,
                    "harness":provider.program(),"created_at_unix":1
                }))
                .unwrap()],
                unix_now(),
            );
            assert_eq!(sampled[&shell].activity, AgentActivity::Exited);
            let ledger = || -> Value {
                serde_json::from_slice(&fs::read(home.join("orchestrator-wakes.json")).unwrap())
                    .unwrap()
            };
            assert!(
                ledger()["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["outcome"] == "interrupted")
            );
            if provider == HarnessKind::Grok {
                record("UserPromptSubmit", "turn-c", json!({}));
                record("StopFailure", "turn-c", json!({}));
                assert_eq!(state(), AgentActivity::Done);
                assert!(
                    ledger()["entries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| entry["outcome"] == "failed")
                );
                let before = ledger()["entries"].as_array().unwrap().len();
                for (event, extra) in [
                    ("Notification", json!({"notificationType":"idle_prompt"})),
                    (
                        "Stop",
                        json!({"prompt_id":null,"promptId":null,"reason":"future_end_reason"}),
                    ),
                    (
                        "Stop",
                        json!({"prompt_id":null,"promptId":null,"reason":"channel_closed"}),
                    ),
                    (
                        "Stop",
                        json!({"prompt_id":null,"promptId":null,"reason":"shutdown"}),
                    ),
                    ("StopCancelled", json!({"prompt_id":null,"promptId":null})),
                    ("StopFailure", json!({"prompt_id":null,"promptId":null})),
                    ("SessionEnd", json!({"prompt_id":null,"promptId":null})),
                ] {
                    record("UserPromptSubmit", "orphan", json!({}));
                    record(event, "orphan", extra);
                    assert_eq!(state(), AgentActivity::Done, "{event}");
                    assert_eq!(
                        ledger()["entries"].as_array().unwrap().len(),
                        before,
                        "unscoped end or idle must not wake an old turn"
                    );
                }
                assert_eq!(
                    grok_hooks(Path::new("/riwork"), &home, &shell)["Notification"][0]["matcher"],
                    "^(permission_prompt|idle_prompt)$"
                );
            }
            record("UserPromptSubmit", "quiet-turn", json!({}));
            let path = home
                .join("agent-hooks")
                .join(provider.program())
                .join(format!("{shell}.json"));
            let mut cursor = read_cursor(&path).unwrap();
            cursor.seen = 100;
            cursor.since = 100;
            save_cursor(&path, &cursor).unwrap();
            assert_eq!(
                harness_snapshot(&home, &shell, provider, 100 + TURN_QUIET_SECS, true)
                    .unwrap()
                    .0
                    .activity,
                AgentActivity::Working
            );
            assert_eq!(
                harness_snapshot(&home, &shell, provider, 101 + TURN_QUIET_SECS, true)
                    .unwrap()
                    .0
                    .activity,
                AgentActivity::Done
            );
            let before = ledger()["entries"].as_array().unwrap().len();
            harness_snapshot(&home, &shell, provider, 9999, true).unwrap();
            assert_eq!(
                ledger()["entries"].as_array().unwrap().len(),
                before,
                "quiet timeout is deduplicated"
            );
            fs::remove_dir_all(home).unwrap();
        }
    }

    #[test]
    fn trust_hash_matches_codex_01621_version_for_toml_vector() {
        // Fixed upstream-normalized TOML identity:
        // event_name = "interrupt"
        // [[hooks]] type="command", command="/usr/bin/true", timeout=3, async=false
        // None matcher/commandWindows/statusMessage fields are omitted by TOML.
        assert_eq!(
            trust_hash(
                "interrupt",
                json!({"command":"/usr/bin/true","type":"command","async":false,"timeout":3})
            ),
            "sha256:5d6db0f71566d5b835e4197644ccec58d1d396043ce1e4b30f804571d0c2a60e"
        );
    }

    #[test]
    fn codex_trust_is_scoped_to_our_exact_session_handlers() {
        let shell = "00000000-0000-4000-8000-000000000001";
        let args = codex_arguments(
            Path::new("/test/riwork's binary"),
            Path::new("/test/state"),
            shell,
        );
        let doc: toml_edit::DocumentMut = args[1].parse().unwrap();
        let hooks = doc["hooks"].as_inline_table().unwrap();
        assert_eq!(
            hooks["Interrupt"]
                .as_array()
                .unwrap()
                .get(0)
                .unwrap()
                .as_inline_table()
                .unwrap()["hooks"]
                .as_array()
                .unwrap()
                .get(0)
                .unwrap()
                .as_inline_table()
                .unwrap()["timeout"]
                .as_integer(),
            Some(3)
        );
        let states = hooks["state"].as_inline_table().unwrap();
        assert_eq!(states.len(), CODEX_EVENTS.len());
        assert!(states.iter().all(|(key, value)| {
            key.starts_with("/<session-flags>/config.toml:")
                && value.as_inline_table().unwrap()["trusted_hash"]
                    .as_str()
                    .unwrap()
                    .starts_with("sha256:")
        }));
        assert!(!args[1].contains("bypass-hook-trust"));
        assert_ne!(
            args,
            codex_arguments(
                Path::new("/changed/riwork"),
                Path::new("/test/state"),
                shell
            )
        );
    }
}
