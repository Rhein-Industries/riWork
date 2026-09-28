//! Claude completion hooks retain identifiers only, never prompts or replies.

use std::{
    fs,
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
};

use fs2::FileExt;
use serde::{Deserialize, Serialize, de::IgnoredAny};
use uuid::Uuid;

use crate::sessions::{HarnessKind, SessionManager};

/// Events RiWork hooks into every Claude launch, through per-invocation
/// `--settings` only. Stop and UserPromptSubmit track the turn; SessionStart
/// rebinds the identity after /clear or /resume; SubagentStop proves a turn is
/// running when a UserPromptSubmit was missed.
pub(crate) const CLAUDE_HOOK_EVENTS: [&str; 4] =
    ["UserPromptSubmit", "Stop", "SessionStart", "SubagentStop"];
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_CURSOR_BYTES: u64 = 16 * 1024;

#[derive(Deserialize)]
struct ClaudeHookInput {
    session_id: String,
    #[serde(default)]
    prompt_id: Option<String>,
    hook_event_name: String,
    /// SessionStart only: startup, resume, clear or compact.
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    agent_id: Option<IgnoredAny>,
    #[serde(default)]
    agent_transcript_path: Option<IgnoredAny>,
    #[serde(default)]
    stop_hook_active: bool,
    #[serde(default)]
    background_tasks: Vec<IgnoredAny>,
    #[serde(default)]
    session_crons: Vec<IgnoredAny>,
}

#[derive(Default, Deserialize, Serialize)]
struct ClaudeTurnCursor {
    session_id: String,
    turn_id: String,
    completed: bool,
}

fn valid_identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_:".contains(&byte))
}

impl ClaudeTurnCursor {
    fn observe(&mut self, input: &ClaudeHookInput) -> Option<String> {
        if !valid_identifier(&input.session_id)
            || input
                .prompt_id
                .as_deref()
                .is_some_and(|id| !valid_identifier(id))
        {
            return None;
        }
        if input.hook_event_name == "SubagentStop" {
            // A subagent just finished, so this session's main agent is
            // mid-turn even if its UserPromptSubmit was missed. Drop any stale
            // completion; the turn's own Stop completes it again. Never complete.
            if self.session_id == input.session_id {
                self.completed = false;
            }
            return None;
        }
        if input.agent_id.is_some() || input.agent_transcript_path.is_some() {
            return None;
        }
        match input.hook_event_name.as_str() {
            "SessionStart" => {
                // /clear, /resume and a restart begin a different conversation;
                // the previous one's completed turn says nothing about it and
                // must not admit a scheduled prompt. The identity is rebound so
                // a schedule pinned to the old session sees it changed.
                // Compaction keeps the session and its turn lifecycle.
                if !(input.source.as_deref() == Some("compact")
                    && self.session_id == input.session_id)
                {
                    *self = Self {
                        session_id: input.session_id.clone(),
                        turn_id: String::new(),
                        completed: false,
                    };
                }
                None
            }
            "UserPromptSubmit" => {
                let turn = input
                    .prompt_id
                    .clone()
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                // Repeated delivery of a known prompt cannot reopen its Stop.
                if self.session_id != input.session_id || self.turn_id != turn {
                    *self = Self {
                        session_id: input.session_id.clone(),
                        turn_id: turn,
                        completed: false,
                    };
                }
                None
            }
            "Stop"
                if !input.stop_hook_active
                    && input.background_tasks.is_empty()
                    && input.session_crons.is_empty() =>
            {
                // A rebound session (SessionStart) has no turn yet but still
                // rejects a late Stop that belongs to the replaced session.
                if (!self.session_id.is_empty() && self.session_id != input.session_id)
                    || (!self.turn_id.is_empty()
                        && input
                            .prompt_id
                            .as_ref()
                            .is_some_and(|turn| turn != &self.turn_id))
                {
                    return None;
                }
                let turn = match &input.prompt_id {
                    Some(turn) => turn.clone(),
                    None if self.session_id == input.session_id && !self.turn_id.is_empty() => {
                        self.turn_id.clone()
                    }
                    // Older Claude versions need a fresh UserPromptSubmit;
                    // a random timestamp at Stop would defeat deduplication.
                    None => return None,
                };
                if self.session_id == input.session_id && self.turn_id == turn && self.completed {
                    return None;
                }
                *self = Self {
                    session_id: input.session_id.clone(),
                    turn_id: turn.clone(),
                    completed: true,
                };
                Some(
                    serde_json::to_string(&("claude", &input.session_id, turn))
                        .expect("identifiers serialize"),
                )
            }
            _ => None,
        }
    }
}

pub fn record_claude_hook(home: &Path, shell_id: &str, input: &str) -> Result<(), String> {
    if input.len() > MAX_INPUT_BYTES {
        return Err("Claude completion hook input exceeds 1 MiB".into());
    }
    let input: ClaudeHookInput =
        serde_json::from_str(input).map_err(|_| "Invalid Claude completion hook input")?;
    let manager = SessionManager::at(home.to_path_buf())?;
    let session = manager.registered_session(shell_id)?;
    if session.harness != Some(HarnessKind::Claude) || session.project_id.is_none() {
        return Ok(());
    }
    let directory = home.join("agent-hooks/claude");
    fs::create_dir_all(&directory).map_err(|_| "Cannot create Claude completion cursors")?;
    let lock = private_file(&directory.join(format!("{shell_id}.lock")), false)?;
    FileExt::lock_exclusive(&lock).map_err(|_| "Cannot lock Claude completion cursor")?;
    let path = directory.join(format!("{shell_id}.json"));
    if fs::symlink_metadata(&path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err("Claude completion cursor must be a regular file".into());
    }
    let mut cursor = match OpenOptions::new().read(true).open(&path) {
        Ok(file) => {
            if file
                .metadata()
                .map_err(|_| "Cannot inspect Claude completion cursor")?
                .len()
                > MAX_CURSOR_BYTES
            {
                return Err("Claude completion cursor exceeds the limit".into());
            }
            let mut bytes = Vec::new();
            file.take(MAX_CURSOR_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "Cannot read Claude completion cursor")?;
            serde_json::from_slice::<ClaudeTurnCursor>(&bytes)
                .map_err(|_| "Invalid Claude completion cursor")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ClaudeTurnCursor::default(),
        Err(_) => return Err("Cannot read Claude completion cursor".into()),
    };
    let event = cursor.observe(&input);
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        // The shared queue owns deduplication. Keep the previous cursor if it
        // cannot enqueue, so a repeated hook can safely retry this completion.
        if let Some(event) = &event {
            crate::notifications::record_completion(home, shell_id, event, HarnessKind::Claude)?;
        }
        let mut file = private_file(&temporary, true)?;
        serde_json::to_writer(&mut file, &cursor)
            .map_err(|_| "Cannot save Claude completion cursor")?;
        file.write_all(b"\n")
            .and_then(|_| file.sync_all())
            .map_err(|_| "Cannot save Claude completion cursor")?;
        fs::rename(&temporary, &path).map_err(|_| "Cannot install Claude completion cursor")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Structured Claude lifecycle gate; terminal prompt evidence is also required.
pub(crate) fn schedule_state(home: &Path, shell_id: &str) -> Option<(String, Option<String>)> {
    let file = fs::File::open(
        home.join("agent-hooks/claude")
            .join(format!("{shell_id}.json")),
    )
    .ok()?;
    if file.metadata().ok()?.len() > MAX_CURSOR_BYTES {
        return None;
    }
    let cursor: ClaudeTurnCursor = serde_json::from_reader(file.take(MAX_CURSOR_BYTES + 1)).ok()?;
    if !valid_identifier(&cursor.session_id) {
        return None;
    }
    let token = (cursor.completed && valid_identifier(&cursor.turn_id)).then_some(cursor.turn_id);
    Some((cursor.session_id, token))
}

fn private_file(path: &Path, create_new: bool) -> Result<fs::File, String> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err("Claude completion cursor must be a regular file".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| "Cannot create Claude completion cursor".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        home: std::path::PathBuf,
        shell_id: String,
        project_id: String,
    }

    impl Fixture {
        fn new() -> Self {
            let home = std::env::temp_dir().join(format!("riwork-claude-hooks-{}", Uuid::new_v4()));
            fs::create_dir_all(home.join("project")).unwrap();
            let store = crate::store::Store::open(home.clone()).unwrap();
            let project = store
                .add_project(home.join("project"), Some("Hook project"))
                .unwrap();
            let shell_id = Uuid::new_v4().to_string();
            fs::write(
                home.join("sessions.json"),
                serde_json::json!({"sessions":[{
                    "id":shell_id,"project_id":project.id,"worktree_id":null,
                    "kind":"project","cwd":project.root,"command":null,
                    "harness":"claude","created_at_unix":1
                }]})
                .to_string(),
            )
            .unwrap();
            Self {
                home,
                shell_id,
                project_id: project.id,
            }
        }
        fn enabled(&self, enabled: bool) {
            crate::store::Store::open(self.home.clone())
                .unwrap()
                .set_project_notifications(&self.project_id, enabled)
                .unwrap();
        }
        fn payload(event: &str, prompt: &str) -> String {
            serde_json::json!({
                "hook_event_name":event,"session_id":"session-a","prompt_id":prompt,
                "prompt":"private request","last_assistant_message":"private response"
            })
            .to_string()
        }
        fn record(&self, event: &str, prompt: &str) -> Result<(), String> {
            record_claude_hook(&self.home, &self.shell_id, &Self::payload(event, prompt))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn concurrent_stops_queue_one_alert_and_retain_only_private_identifiers() {
        let fixture = Fixture::new();
        fixture.enabled(true);
        fixture.record("UserPromptSubmit", "prompt-a").unwrap();
        let mut workers = Vec::new();
        for _ in 0..4 {
            let home = fixture.home.clone();
            let shell = fixture.shell_id.clone();
            workers.push(std::thread::spawn(move || {
                record_claude_hook(&home, &shell, &Fixture::payload("Stop", "prompt-a"))
            }));
        }
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        assert_eq!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .len(),
            1
        );
        assert!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .is_empty()
        );
        let path = fixture
            .home
            .join("agent-hooks/claude")
            .join(format!("{}.json", fixture.shell_id));
        let saved = fs::read_to_string(&path).unwrap();
        assert!(
            !saved.contains("private")
                && !saved.contains("prompt\":")
                && !saved.contains("response")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn enabling_does_not_replay_an_old_stop_and_failed_enqueues_can_retry() {
        let fixture = Fixture::new();
        fixture.record("UserPromptSubmit", "old").unwrap();
        fixture.record("Stop", "old").unwrap();
        fixture.enabled(true);
        fixture.record("Stop", "old").unwrap();
        assert!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .is_empty()
        );
        fixture.record("UserPromptSubmit", "new").unwrap();
        fs::write(fixture.home.join("agent-notifications.json"), "broken").unwrap();
        assert!(fixture.record("Stop", "new").is_err());
        fs::remove_file(fixture.home.join("agent-notifications.json")).unwrap();
        fixture.record("Stop", "new").unwrap();
        assert_eq!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .len(),
            1
        );
        assert!(fixture.record("Stop", "new").is_ok());
        assert!(
            crate::notifications::claim_pending(&fixture.home)
                .unwrap()
                .is_empty()
        );
    }

    fn hook(event: &str, session: &str, prompt: Option<&str>) -> ClaudeHookInput {
        serde_json::from_value(serde_json::json!({
            "hook_event_name":event,"session_id":session,"prompt_id":prompt,
            "prompt":"private user prompt","last_assistant_message":"private assistant response"
        }))
        .unwrap()
    }

    #[test]
    fn real_prompt_ids_deduplicate_stop_without_persisting_messages() {
        let mut cursor = ClaudeTurnCursor::default();
        let stop = hook("Stop", "session-a", Some("prompt-a"));
        let key = cursor.observe(&stop).unwrap();
        assert!(cursor.observe(&stop).is_none());
        cursor.observe(&hook("UserPromptSubmit", "session-a", Some("prompt-a")));
        assert!(cursor.observe(&stop).is_none());
        cursor.observe(&hook("UserPromptSubmit", "session-a", Some("prompt-b")));
        assert!(cursor.observe(&stop).is_none());
        assert_ne!(
            key,
            cursor
                .observe(&hook("Stop", "session-a", Some("prompt-b")))
                .unwrap()
        );
        let saved = serde_json::to_string(&cursor).unwrap();
        assert!(
            !saved.contains("private")
                && !saved.contains("assistant_message")
                && !saved.contains("transcript")
        );
    }

    #[test]
    fn older_versions_require_a_submitted_turn_and_keep_one_synthetic_key() {
        let mut cursor = ClaudeTurnCursor::default();
        let stop = hook("Stop", "session-a", None);
        assert!(cursor.observe(&stop).is_none());
        cursor.observe(&hook("UserPromptSubmit", "session-a", None));
        let first = cursor.observe(&stop).unwrap();
        assert!(cursor.observe(&stop).is_none());
        assert!(
            cursor
                .observe(&hook("Stop", "other-session", None))
                .is_none()
        );
        cursor.observe(&hook("UserPromptSubmit", "session-a", None));
        assert_ne!(first, cursor.observe(&stop).unwrap());
    }

    #[test]
    fn incomplete_failed_and_subagent_events_do_not_complete_a_turn() {
        let mut cursor = ClaudeTurnCursor::default();
        for event in ["StopFailure", "SubagentStop", "SessionEnd", "Notification"] {
            assert!(
                cursor
                    .observe(&hook(event, "session-a", Some("prompt-a")))
                    .is_none()
            );
        }
        let mut stop = hook("Stop", "session-a", Some("prompt-a"));
        stop.stop_hook_active = true;
        assert!(cursor.observe(&stop).is_none());
        stop.stop_hook_active = false;
        stop.background_tasks = serde_json::from_str("[{}]").unwrap();
        assert!(cursor.observe(&stop).is_none());
        stop.background_tasks.clear();
        stop.session_crons = serde_json::from_str("[{}]").unwrap();
        assert!(cursor.observe(&stop).is_none());
        stop.session_crons.clear();
        stop.agent_id = serde_json::from_str("\"subagent\"").ok();
        assert!(cursor.observe(&stop).is_none());
    }

    fn session_start(session: &str, source: &str) -> ClaudeHookInput {
        serde_json::from_value(serde_json::json!({
            "hook_event_name":"SessionStart","session_id":session,"source":source,
            "cwd":"/private/project","transcript_path":"/private/transcript.jsonl"
        }))
        .unwrap()
    }

    fn completed(cursor: &ClaudeTurnCursor) -> (String, bool) {
        (cursor.session_id.clone(), cursor.completed)
    }

    #[test]
    fn session_start_rebinds_a_replaced_conversation_but_not_a_compaction() {
        let mut cursor = ClaudeTurnCursor::default();
        cursor.observe(&hook("UserPromptSubmit", "session-a", Some("prompt-a")));
        cursor.observe(&hook("Stop", "session-a", Some("prompt-a")));
        assert_eq!(completed(&cursor), ("session-a".into(), true));
        // Compaction keeps the conversation and its lifecycle.
        assert!(
            cursor
                .observe(&session_start("session-a", "compact"))
                .is_none()
        );
        assert_eq!(completed(&cursor), ("session-a".into(), true));
        // /clear and /resume start another conversation: the old completion
        // must not stay ready, and a late Stop of the old session is ignored.
        for source in ["clear", "resume", "startup"] {
            let mut cursor = ClaudeTurnCursor::default();
            cursor.observe(&hook("UserPromptSubmit", "session-a", Some("prompt-a")));
            cursor.observe(&hook("Stop", "session-a", Some("prompt-a")));
            assert!(
                cursor
                    .observe(&session_start("session-b", source))
                    .is_none()
            );
            assert_eq!(completed(&cursor), ("session-b".into(), false), "{source}");
            assert!(
                cursor
                    .observe(&hook("Stop", "session-a", Some("prompt-a")))
                    .is_none()
            );
            assert_eq!(completed(&cursor), ("session-b".into(), false), "{source}");
            // The new conversation completes normally, and only once.
            cursor.observe(&hook("UserPromptSubmit", "session-b", Some("prompt-b")));
            assert!(
                cursor
                    .observe(&hook("Stop", "session-b", Some("prompt-b")))
                    .is_some()
            );
            assert_eq!(completed(&cursor), ("session-b".into(), true));
        }
        // A compaction that reports another session id is a replacement.
        assert!(
            cursor
                .observe(&session_start("session-c", "compact"))
                .is_none()
        );
        assert_eq!(completed(&cursor), ("session-c".into(), false));
        // A fresh process resuming the same id starts without a completion.
        cursor.observe(&hook("UserPromptSubmit", "session-c", Some("prompt-c")));
        cursor.observe(&hook("Stop", "session-c", Some("prompt-c")));
        cursor.observe(&session_start("session-c", "resume"));
        assert_eq!(completed(&cursor), ("session-c".into(), false));
    }

    #[test]
    fn subagent_stop_withdraws_a_stale_completion_and_never_completes() {
        let mut cursor = ClaudeTurnCursor::default();
        cursor.observe(&hook("UserPromptSubmit", "session-a", Some("prompt-a")));
        assert!(
            cursor
                .observe(&hook("Stop", "session-a", Some("prompt-a")))
                .is_some()
        );
        let mut subagent = hook("SubagentStop", "session-a", Some("prompt-a"));
        subagent.agent_id = serde_json::from_str("\"subagent\"").ok();
        subagent.agent_transcript_path = serde_json::from_str("\"/private/agent.jsonl\"").ok();
        assert!(cursor.observe(&subagent).is_none());
        assert_eq!(completed(&cursor), ("session-a".into(), false));
        // Another session's subagent says nothing about this cursor.
        cursor.observe(&hook("Stop", "session-a", Some("prompt-a")));
        subagent.session_id = "session-z".into();
        cursor.observe(&subagent);
        assert_eq!(completed(&cursor), ("session-a".into(), true));
        // A subagent finishing never completes a turn by itself.
        let mut fresh = ClaudeTurnCursor::default();
        subagent.session_id = "session-a".into();
        assert!(fresh.observe(&subagent).is_none());
        assert!(!fresh.completed);
    }

    #[test]
    fn session_start_and_turn_events_reach_the_saved_cursor() {
        let fixture = Fixture::new();
        let write = |payload: serde_json::Value| {
            record_claude_hook(&fixture.home, &fixture.shell_id, &payload.to_string()).unwrap()
        };
        let start = |session: &str, source: &str| serde_json::json!({"hook_event_name":"SessionStart","session_id":session,"source":source});
        write(start("session-a", "startup"));
        assert_eq!(
            schedule_state(&fixture.home, &fixture.shell_id),
            Some(("session-a".into(), None)),
            "a started session is identified before its first turn"
        );
        fixture.record("UserPromptSubmit", "prompt-a").unwrap();
        fixture.record("Stop", "prompt-a").unwrap();
        assert_eq!(
            schedule_state(&fixture.home, &fixture.shell_id),
            Some(("session-a".into(), Some("prompt-a".into())))
        );
        write(start("session-b", "clear"));
        assert_eq!(
            schedule_state(&fixture.home, &fixture.shell_id),
            Some(("session-b".into(), None))
        );
    }
}
