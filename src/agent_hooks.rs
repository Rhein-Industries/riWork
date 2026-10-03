//! Claude hooks retain identifiers only, never prompts or replies. They keep
//! one small cursor per shell: which conversation and turn the pane is in,
//! whether the turn completed, since when, and which subagents have started
//! and not yet stopped. `claude_state` turns the cursor into the activity the
//! desktop and the phone show.

use std::{
    fs,
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
};

use fs2::FileExt;
use serde::{Deserialize, Serialize, de::IgnoredAny};
use uuid::Uuid;

use crate::{
    activity::{AgentActivity, AgentState, SUBAGENT_STALE_SECS, Subagents, unix_now},
    sessions::{HarnessKind, SessionManager},
};

/// Events RiWork hooks into every Claude launch, through per-invocation
/// `--settings` only. Stop and UserPromptSubmit track the turn; SessionStart
/// rebinds the identity after /clear or /resume; SubagentStart and
/// SubagentStop pair up by `agent_id` to count the subagents a turn is
/// running, and SubagentStop also proves a turn is running when a
/// UserPromptSubmit was missed.
pub(crate) const CLAUDE_HOOK_EVENTS: [&str; 5] = [
    "UserPromptSubmit",
    "Stop",
    "SessionStart",
    "SubagentStart",
    "SubagentStop",
];
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_CURSOR_BYTES: u64 = 16 * 1024;
/// Subagents remembered per shell. A cursor stays far below `MAX_CURSOR_BYTES`
/// with this many, whatever their names.
const MAX_SUBAGENTS: usize = 32;
/// An open turn that has shown no sign of life for this long reads as waiting.
/// Claude sends no hook when a turn is interrupted with Esc, and a subagent
/// killed with it never sends its SubagentStop, so silence is the only evidence.
/// A sign of life is a hook event or a refresh of the status line, which Claude
/// runs after every message.
const TURN_QUIET_SECS: u64 = 10 * 60;
const MAX_SUBAGENT_ID_BYTES: usize = 64;
const MAX_KIND_BYTES: usize = 40;

/// A hook field that should be text. Anything else is tolerated and ignored,
/// so a Claude version that changes a field cannot make every hook fail.
#[derive(Deserialize)]
#[serde(untagged)]
enum Text {
    Text(String),
    Other(IgnoredAny),
}

impl Text {
    fn get(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Other(_) => None,
        }
    }
}

/// One entry of a Stop hook's `background_tasks`. Only these four fields are
/// read: the entry's `description` is free text and is never looked at.
#[derive(Deserialize)]
#[serde(untagged)]
enum BackgroundTask {
    Entry {
        id: Option<String>,
        #[serde(rename = "type")]
        kind: Option<String>,
        status: Option<String>,
        agent_type: Option<String>,
    },
    Other(IgnoredAny),
}

impl BackgroundTask {
    /// The id and kind of a subagent still running, if this entry is one.
    fn running_subagent(&self) -> Option<(&str, Option<&str>)> {
        match self {
            Self::Entry {
                id: Some(id),
                kind: Some(kind),
                status,
                agent_type,
            } if kind == "subagent"
                && status.as_deref().is_none_or(|status| status == "running") =>
            {
                Some((id, agent_type.as_deref()))
            }
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct ClaudeHookInput {
    session_id: String,
    #[serde(default)]
    prompt_id: Option<String>,
    hook_event_name: String,
    /// SessionStart only: startup, resume, clear or compact.
    #[serde(default)]
    source: Option<String>,
    /// Present on events that fire inside a subagent, and on its start and stop.
    #[serde(default)]
    agent_id: Option<Text>,
    /// The kind of subagent: a name such as `general-purpose` or `Explore`.
    #[serde(default)]
    agent_type: Option<Text>,
    #[serde(default)]
    agent_transcript_path: Option<IgnoredAny>,
    #[serde(default)]
    stop_hook_active: bool,
    #[serde(default)]
    background_tasks: Vec<BackgroundTask>,
    #[serde(default)]
    session_crons: Vec<IgnoredAny>,
}

#[derive(Default, Deserialize, Serialize)]
struct ClaudeTurnCursor {
    session_id: String,
    turn_id: String,
    completed: bool,
    /// Unix seconds at which the current phase began: the turn's start, its
    /// Stop, or the session's start. Zero in a cursor an older build wrote.
    #[serde(default, skip_serializing_if = "is_zero")]
    since_unix: u64,
    /// Unix seconds of the last hook event this cursor took in.
    #[serde(default, skip_serializing_if = "is_zero")]
    seen_unix: u64,
    /// When a Stop that listed background work paused the turn without ending
    /// it: Claude is back at its prompt, and will be woken when the work is done.
    #[serde(default, skip_serializing_if = "is_zero")]
    paused_unix: u64,
    /// Subagents started and not yet stopped, in start order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    subagents: Vec<RunningSubagent>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct RunningSubagent {
    id: String,
    /// `agent_type`, a name; absent when it was empty or not a plain name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    started_unix: u64,
}

fn valid_identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_:".contains(&byte))
}

/// A subagent kind or role as it is shown: a short plain name.
pub(crate) fn valid_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= MAX_KIND_BYTES
        && kind
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_:.".contains(&byte))
}

impl ClaudeTurnCursor {
    /// A cursor for a conversation's new phase, with nothing running in it.
    fn begin(session_id: &str, turn_id: String, completed: bool, now: u64) -> Self {
        Self {
            session_id: session_id.to_owned(),
            turn_id,
            completed,
            since_unix: now,
            seen_unix: now,
            paused_unix: 0,
            subagents: Vec::new(),
        }
    }

    fn observe(&mut self, input: &ClaudeHookInput) -> Option<String> {
        self.observe_at(input, unix_now())
    }

    fn observe_at(&mut self, input: &ClaudeHookInput, now: u64) -> Option<String> {
        if !valid_identifier(&input.session_id)
            || input
                .prompt_id
                .as_deref()
                .is_some_and(|id| !valid_identifier(id))
        {
            return None;
        }
        self.expire(now);
        self.seen_unix = now;
        let agent = input.agent_id.as_ref().and_then(Text::get);
        match input.hook_event_name.as_str() {
            "SubagentStart" => {
                self.subagent_started(input, now);
                return None;
            }
            "SubagentStop" => {
                // A stop that counts as subagent activity proves the main agent is
                // mid-turn, so a completion left from an earlier turn is withdrawn
                // (the turn's own Stop completes it again). One that does not is
                // ignored. A stop never completes a turn.
                if self.session_id == input.session_id {
                    if self.stop_counts_as_activity(input) {
                        self.reopen(now);
                    }
                    if let Some(agent) = agent {
                        self.subagents.retain(|running| running.id != agent);
                    }
                }
                return None;
            }
            _ => {}
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
                    *self = Self::begin(&input.session_id, String::new(), false, now);
                }
                None
            }
            "UserPromptSubmit" => {
                let turn = input
                    .prompt_id
                    .clone()
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                // Repeated delivery of a known prompt cannot reopen its Stop.
                // A new turn starts without the previous one's subagents: they
                // belong to it, and an interrupted turn never says they ended.
                // A background subagent still running comes back at the next
                // Stop, which lists it.
                if self.session_id != input.session_id || self.turn_id != turn {
                    *self = Self::begin(&input.session_id, turn, false, now);
                }
                None
            }
            "Stop" if !input.stop_hook_active => {
                // Whatever the Stop lists as running is what runs, whether or
                // not this Stop ends the turn.
                if self.session_id == input.session_id {
                    self.reconcile(&input.background_tasks, now);
                }
                if !input.background_tasks.is_empty() || !input.session_crons.is_empty() {
                    // Claude is back at its prompt but the turn is not over: it
                    // is paused until the background work wakes it.
                    if self.session_id == input.session_id
                        && input
                            .prompt_id
                            .as_ref()
                            .is_none_or(|turn| self.turn_id.is_empty() || turn == &self.turn_id)
                    {
                        self.paused_unix = now;
                    }
                    return None;
                }
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
                // The turn is over, and with it every subagent it started.
                *self = Self::begin(&input.session_id, turn.clone(), true, now);
                Some(
                    serde_json::to_string(&("claude", &input.session_id, turn))
                        .expect("identifiers serialize"),
                )
            }
            _ => None,
        }
    }

    /// The activity the cursor stands for. `status_line` is when Claude last
    /// refreshed its status line, which it does after every message.
    ///
    /// - a turn that is open and has shown a sign of life lately, or a subagent
    ///   that is running: working;
    /// - a turn whose Stop arrived: done;
    /// - otherwise waiting: a session that started and has not been prompted, a
    ///   turn paused by a Stop that listed background work, or a turn that has
    ///   gone quiet (see `TURN_QUIET_SECS`).
    ///
    /// Subagents are reported only while the pane is working, and not once they
    /// are `SUBAGENT_STALE_SECS` old.
    fn state(&self, now: u64, status_line: Option<u64>) -> AgentState {
        let mut subagents = Subagents::default();
        let mut subagents_since = u64::MAX;
        for running in &self.subagents {
            if now.saturating_sub(running.started_unix) < SUBAGENT_STALE_SECS {
                subagents.add(running.kind.as_deref());
                subagents_since = subagents_since.min(running.started_unix);
            }
        }
        let last_sign = self.seen_unix.max(status_line.unwrap_or(0));
        let quiet = last_sign > 0 && now.saturating_sub(last_sign) > TURN_QUIET_SECS;
        let turn_open = !self.completed && !self.turn_id.is_empty();
        let turn_working = turn_open && self.paused_unix == 0 && !quiet;
        let (activity, since) = if turn_working {
            (AgentActivity::Working, self.since_unix)
        } else if subagents.working > 0 {
            (AgentActivity::Working, subagents_since)
        } else if self.completed {
            (AgentActivity::Done, self.since_unix)
        } else if self.paused_unix > 0 {
            (AgentActivity::Waiting, self.paused_unix)
        } else if turn_open {
            (AgentActivity::Waiting, last_sign)
        } else {
            (AgentActivity::Waiting, self.since_unix)
        };
        AgentState {
            activity,
            since_unix: (since > 0).then_some(since),
            subagents: if activity == AgentActivity::Working {
                subagents
            } else {
                Subagents::default()
            },
        }
    }

    /// Subagents that have gone unheard of for `SUBAGENT_STALE_SECS` are
    /// dropped: an interrupted turn sends neither their SubagentStop nor a Stop.
    fn expire(&mut self, now: u64) {
        self.subagents
            .retain(|running| now.saturating_sub(running.started_unix) < SUBAGENT_STALE_SECS);
    }

    /// The hook names a prompt the cursor never saw begin: the UserPromptSubmit
    /// of a turn that is running was missed.
    fn names_unseen_turn(&self, input: &ClaudeHookInput) -> bool {
        input
            .prompt_id
            .as_deref()
            .is_some_and(|turn| turn != self.turn_id)
    }

    /// Whether a subagent hook shows that the main agent is mid-turn. This is the
    /// one rule behind both what the app shows and whether a scheduled prompt may
    /// be sent, so the two cannot disagree.
    ///
    /// A SubagentStop counts when its `agent_id` pairs with a SubagentStart seen
    /// for this conversation, or when it names a prompt the cursor never saw begin.
    /// The second needs no SubagentStart, so it also holds for a Claude launched
    /// before that hook was registered, which can only ever send unpaired stops.
    /// A stop that is neither does not count. Claude 2.1.288 sends one, or two,
    /// 3 to 5 seconds after some replies (tool-using or not): no start came
    /// before it, its `agent_type` is empty and it carries the finished turn's
    /// own prompt, so it says nothing about whether a turn is running. (Likely
    /// the end of the prompt suggestion Claude makes after a reply; that is a
    /// guess, the rule rests only on what the hook carries.)
    fn stop_counts_as_activity(&self, input: &ClaudeHookInput) -> bool {
        let paired = input
            .agent_id
            .as_ref()
            .and_then(Text::get)
            .is_some_and(|agent| self.subagents.iter().any(|running| running.id == agent));
        paired || self.names_unseen_turn(input)
    }

    /// Withdraws a completion because a turn is running again.
    fn reopen(&mut self, now: u64) {
        if self.completed {
            self.completed = false;
            self.since_unix = now;
        }
    }

    /// A subagent of this conversation's current turn began. One that starts
    /// after the turn's Stop belongs to no turn Claude is working on and is not
    /// counted, unless it names a prompt the cursor never saw begin.
    fn subagent_started(&mut self, input: &ClaudeHookInput, now: u64) {
        let Some(id) = input
            .agent_id
            .as_ref()
            .and_then(Text::get)
            .filter(|id| valid_identifier(id) && id.len() <= MAX_SUBAGENT_ID_BYTES)
        else {
            return;
        };
        if self.session_id != input.session_id {
            return;
        }
        if self.completed {
            // After the turn's Stop a start belongs to no turn Claude is working
            // on, unless it names a prompt the cursor never saw begin.
            if !self.names_unseen_turn(input) {
                return;
            }
            self.reopen(now);
        }
        let kind = input
            .agent_type
            .as_ref()
            .and_then(Text::get)
            .filter(|kind| valid_kind(kind))
            .map(str::to_owned);
        // SubagentStart repeats when a subagent is resumed, and a resumed
        // subagent means the paused turn is working again.
        self.paused_unix = 0;
        self.subagents.retain(|running| running.id != id);
        self.subagents.push(RunningSubagent {
            id: id.to_owned(),
            kind,
            started_unix: now,
        });
        if self.subagents.len() > MAX_SUBAGENTS {
            self.subagents.remove(0);
        }
    }

    /// Makes the subagent list match a Stop's `background_tasks`, the parent
    /// session's running work: a tracked subagent the list lacks has finished
    /// (its SubagentStop was lost), and a listed one not tracked yet had its
    /// SubagentStart missed. A foreground subagent cannot outlive its turn's Stop.
    fn reconcile(&mut self, tasks: &[BackgroundTask], now: u64) {
        let running: Vec<_> = tasks
            .iter()
            .filter_map(BackgroundTask::running_subagent)
            .filter(|(id, _)| valid_identifier(id) && id.len() <= MAX_SUBAGENT_ID_BYTES)
            .collect();
        self.subagents
            .retain(|known| running.iter().any(|(id, _)| *id == known.id));
        for (id, kind) in running {
            if self.subagents.len() < MAX_SUBAGENTS
                && !self.subagents.iter().any(|known| known.id == id)
            {
                self.subagents.push(RunningSubagent {
                    id: id.to_owned(),
                    kind: kind.filter(|kind| valid_kind(kind)).map(str::to_owned),
                    started_unix: now,
                });
            }
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

/// The cursor a shell's hooks keep, if there is a usable one.
fn read_cursor(home: &Path, shell_id: &str) -> Option<ClaudeTurnCursor> {
    let file = fs::File::open(
        home.join("agent-hooks/claude")
            .join(format!("{shell_id}.json")),
    )
    .ok()?;
    if file.metadata().ok()?.len() > MAX_CURSOR_BYTES {
        return None;
    }
    let cursor: ClaudeTurnCursor = serde_json::from_reader(file.take(MAX_CURSOR_BYTES + 1)).ok()?;
    valid_identifier(&cursor.session_id).then_some(cursor)
}

/// Structured Claude lifecycle gate; terminal prompt evidence is also required.
pub(crate) fn schedule_state(home: &Path, shell_id: &str) -> Option<(String, Option<String>)> {
    let cursor = read_cursor(home, shell_id)?;
    let token = (cursor.completed && valid_identifier(&cursor.turn_id)).then_some(cursor.turn_id);
    Some((cursor.session_id, token))
}

/// What the hooks say a live Claude pane is doing, or `None` when they have
/// said nothing (a launch from before the hooks, or one that has not started
/// its session yet). See `ClaudeTurnCursor::state`.
pub(crate) fn claude_state(home: &Path, shell_id: &str, now: u64) -> Option<AgentState> {
    let cursor = read_cursor(home, shell_id)?;
    // The status line RiWork gives every launch writes its usage cache on each
    // refresh, so the cache's time says when Claude last drew a message.
    let status_line = crate::usage::read_claude_usage_at(home, shell_id)
        .ok()
        .flatten()
        .map(|usage| usage.updated_at_unix);
    Some(cursor.state(now, status_line))
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
    fn only_a_subagent_stop_that_counts_withdraws_a_completion_and_none_completes() {
        let mut cursor = ClaudeTurnCursor::default();
        cursor.observe(&hook("UserPromptSubmit", "session-a", Some("prompt-a")));
        assert!(
            cursor
                .observe(&hook("Stop", "session-a", Some("prompt-a")))
                .is_some()
        );
        // The stop of a subagent nothing started, naming the finished turn's own
        // prompt (as Claude 2.1.288 sends after some replies), says nothing about
        // a running turn.
        let mut subagent = hook("SubagentStop", "session-a", Some("prompt-a"));
        subagent.agent_id = serde_json::from_str("\"subagent\"").ok();
        subagent.agent_transcript_path = serde_json::from_str("\"/private/agent.jsonl\"").ok();
        assert!(cursor.observe(&subagent).is_none());
        assert_eq!(completed(&cursor), ("session-a".into(), true));
        // The same with no prompt named at all.
        subagent.prompt_id = None;
        cursor.observe(&subagent);
        assert_eq!(completed(&cursor), ("session-a".into(), true));
        // A stop that names a prompt the cursor never saw begin proves a running
        // turn whose UserPromptSubmit was missed, with no start needed (a Claude
        // launched before SubagentStart was registered sends only this kind).
        subagent.prompt_id = Some("prompt-b".into());
        assert!(cursor.observe(&subagent).is_none());
        assert_eq!(completed(&cursor), ("session-a".into(), false));
        // Another session's subagent says nothing about this cursor, named
        // prompt or not.
        cursor.observe(&hook("UserPromptSubmit", "session-a", Some("prompt-a")));
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

    fn subagent_hook(
        event: &str,
        session: &str,
        prompt: Option<&str>,
        agent: &str,
        kind: Option<&str>,
    ) -> ClaudeHookInput {
        serde_json::from_value(serde_json::json!({
            "hook_event_name":event,"session_id":session,"prompt_id":prompt,
            "agent_id":agent,"agent_type":kind,
            "prompt":"private user prompt","last_assistant_message":"private subagent reply"
        }))
        .unwrap()
    }

    fn stop_listing(session: &str, prompt: &str, tasks: serde_json::Value) -> ClaudeHookInput {
        serde_json::from_value(serde_json::json!({
            "hook_event_name":"Stop","session_id":session,"prompt_id":prompt,
            "background_tasks":tasks,"last_assistant_message":"private reply"
        }))
        .unwrap()
    }

    fn working(cursor: &ClaudeTurnCursor, now: u64) -> (AgentActivity, usize, Vec<String>) {
        let state = cursor.state(now, None);
        (
            state.activity,
            state.subagents.working,
            state.subagents.kinds,
        )
    }

    fn open_turn(cursor: &mut ClaudeTurnCursor, now: u64) {
        cursor.observe_at(
            &hook("UserPromptSubmit", "session-a", Some("prompt-a")),
            now,
        );
    }

    #[test]
    fn subagent_starts_and_stops_pair_by_agent_id_and_the_parents_stop_clears_the_rest() {
        let mut cursor = ClaudeTurnCursor::default();
        open_turn(&mut cursor, 100);
        for (agent, kind) in [("agent-1", "general-purpose"), ("agent-2", "Explore")] {
            let start = subagent_hook(
                "SubagentStart",
                "session-a",
                Some("prompt-a"),
                agent,
                Some(kind),
            );
            assert!(cursor.observe_at(&start, 110).is_none());
        }
        assert_eq!(
            working(&cursor, 120),
            (
                AgentActivity::Working,
                2,
                vec!["general-purpose".to_owned(), "Explore".to_owned()]
            )
        );
        // The same subagent starting again (resumed) is still one.
        let again = subagent_hook(
            "SubagentStart",
            "session-a",
            Some("prompt-a"),
            "agent-1",
            Some("general-purpose"),
        );
        cursor.observe_at(&again, 125);
        assert_eq!(working(&cursor, 126).1, 2);
        // A stop pairs with its own start only; an unknown id changes nothing.
        let stop = |agent: &str| {
            subagent_hook(
                "SubagentStop",
                "session-a",
                Some("prompt-a"),
                agent,
                Some("x"),
            )
        };
        cursor.observe_at(&stop("agent-1"), 130);
        assert_eq!(
            working(&cursor, 131),
            (AgentActivity::Working, 1, vec!["Explore".to_owned()])
        );
        cursor.observe_at(&stop("never-started"), 132);
        assert_eq!(working(&cursor, 133).1, 1);
        // A subagent of another conversation is not this one's.
        cursor.observe_at(
            &subagent_hook(
                "SubagentStop",
                "session-z",
                Some("prompt-a"),
                "agent-2",
                None,
            ),
            134,
        );
        assert_eq!(working(&cursor, 135).1, 1);
        // The turn's own Stop ends it, subagents included, whatever they sent.
        assert!(
            cursor
                .observe_at(&hook("Stop", "session-a", Some("prompt-a")), 140)
                .is_some()
        );
        assert_eq!(working(&cursor, 141), (AgentActivity::Done, 0, Vec::new()));
        assert!(cursor.subagents.is_empty());
    }

    #[test]
    fn a_subagent_that_never_stops_expires_and_an_interrupted_turn_goes_quiet() {
        let mut cursor = ClaudeTurnCursor::default();
        open_turn(&mut cursor, 1_000);
        cursor.observe_at(
            &subagent_hook(
                "SubagentStart",
                "session-a",
                Some("prompt-a"),
                "agent-1",
                Some("Plan"),
            ),
            1_010,
        );
        let almost = 1_010 + SUBAGENT_STALE_SECS - 1;
        assert_eq!(working(&cursor, almost).1, 1);
        // Past the timeout it is no longer counted. Nothing has been heard of the
        // turn for as long, so it reads as waiting: the interrupted case.
        let expired = 1_010 + SUBAGENT_STALE_SECS;
        assert_eq!(
            working(&cursor, expired),
            (AgentActivity::Waiting, 0, Vec::new())
        );
        // And the cursor stops carrying it the next time a hook arrives.
        cursor.observe_at(&hook("SessionStart", "session-a", None), expired + 1);
        assert!(cursor.subagents.is_empty());
        let mut again = ClaudeTurnCursor::default();
        open_turn(&mut again, 1_000);
        again.observe_at(
            &subagent_hook(
                "SubagentStart",
                "session-a",
                Some("prompt-a"),
                "agent-1",
                None,
            ),
            1_010,
        );
        again.observe_at(
            &subagent_hook(
                "SubagentStart",
                "session-a",
                Some("prompt-a"),
                "agent-2",
                None,
            ),
            expired,
        );
        assert_eq!(
            again.subagents.len(),
            1,
            "the old one is dropped when the next arrives"
        );
        assert_eq!(again.subagents[0].id, "agent-2");
    }

    #[test]
    fn subagents_belong_to_a_turn_and_a_conversation() {
        let mut cursor = ClaudeTurnCursor::default();
        open_turn(&mut cursor, 100);
        let start = |agent: &str| {
            subagent_hook("SubagentStart", "session-a", Some("prompt-a"), agent, None)
        };
        cursor.observe_at(&start("agent-1"), 110);
        // The same prompt delivered again changes nothing.
        open_turn(&mut cursor, 111);
        assert_eq!(working(&cursor, 112).1, 1);
        // A new prompt starts a turn without the old turn's subagents.
        cursor.observe_at(
            &hook("UserPromptSubmit", "session-a", Some("prompt-b")),
            120,
        );
        assert_eq!(
            working(&cursor, 121),
            (AgentActivity::Working, 0, Vec::new())
        );
        // /clear and /resume rebind and forget them; a compaction keeps them.
        cursor.observe_at(
            &subagent_hook(
                "SubagentStart",
                "session-a",
                Some("prompt-b"),
                "agent-2",
                None,
            ),
            130,
        );
        cursor.observe_at(&session_start("session-a", "compact"), 131);
        assert_eq!(working(&cursor, 132).1, 1);
        cursor.observe_at(&session_start("session-b", "clear"), 133);
        assert_eq!(
            working(&cursor, 134),
            (AgentActivity::Waiting, 0, Vec::new())
        );
        // A start for a conversation the cursor does not hold is ignored.
        cursor.observe_at(&start("agent-3"), 135);
        assert!(cursor.subagents.is_empty());
    }

    #[test]
    fn a_subagent_after_the_turns_stop_is_not_counted_but_one_of_a_newer_turn_is() {
        let mut cursor = ClaudeTurnCursor::default();
        open_turn(&mut cursor, 100);
        cursor.observe_at(&hook("Stop", "session-a", Some("prompt-a")), 110);
        // A start after the Stop that names the finished turn's own prompt belongs
        // to no running turn.
        cursor.observe_at(
            &subagent_hook(
                "SubagentStart",
                "session-a",
                Some("prompt-a"),
                "suggestion",
                Some(""),
            ),
            111,
        );
        assert_eq!(working(&cursor, 112), (AgentActivity::Done, 0, Vec::new()));
        // A start that names a prompt the cursor has not seen means the
        // UserPromptSubmit was missed: that turn is running.
        cursor.observe_at(
            &subagent_hook(
                "SubagentStart",
                "session-a",
                Some("prompt-new"),
                "agent-1",
                Some("Plan"),
            ),
            113,
        );
        assert_eq!(
            working(&cursor, 114),
            (AgentActivity::Working, 1, vec!["Plan".to_owned()])
        );
    }

    #[test]
    fn a_stop_that_lists_background_work_keeps_the_turn_open_and_corrects_the_subagents() {
        let mut cursor = ClaudeTurnCursor::default();
        open_turn(&mut cursor, 100);
        for agent in ["agent-1", "agent-2"] {
            cursor.observe_at(
                &subagent_hook(
                    "SubagentStart",
                    "session-a",
                    Some("prompt-a"),
                    agent,
                    Some("Plan"),
                ),
                110,
            );
        }
        // agent-2 ended without its SubagentStop being seen; agent-3's start was
        // missed; the shell task is not a subagent; a finished one is not running.
        let stop = stop_listing(
            "session-a",
            "prompt-a",
            serde_json::json!([
                {"id":"agent-1","type":"subagent","status":"running","agent_type":"Plan","description":"private"},
                {"id":"agent-3","type":"subagent","status":"running","agent_type":"Explore"},
                {"id":"agent-4","type":"subagent","status":"completed"},
                {"id":"shell-1","type":"shell","status":"running"},
                {"type":"subagent"},
                7,
                {"id":5,"type":"subagent","status":"running"}
            ]),
        );
        assert!(
            cursor.observe_at(&stop, 200).is_none(),
            "the turn is not over"
        );
        assert!(!cursor.completed);
        let ids: Vec<_> = cursor.subagents.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["agent-1", "agent-3"]);
        assert_eq!(
            cursor.subagents[0].started_unix, 110,
            "a known one keeps its start"
        );
        assert_eq!(
            working(&cursor, 201),
            (
                AgentActivity::Working,
                2,
                vec!["Plan".to_owned(), "Explore".to_owned()]
            )
        );
        // When the work ends the turn ends: a Stop that lists nothing completes it.
        let done = cursor.observe_at(&hook("Stop", "session-a", Some("prompt-a")), 300);
        assert!(done.is_some());
        assert_eq!(working(&cursor, 301), (AgentActivity::Done, 0, Vec::new()));
        // A Stop that is about another conversation corrects nothing.
        let mut other = ClaudeTurnCursor::default();
        open_turn(&mut other, 100);
        other.observe_at(
            &subagent_hook(
                "SubagentStart",
                "session-a",
                Some("prompt-a"),
                "agent-1",
                None,
            ),
            110,
        );
        other.observe_at(
            &stop_listing("session-z", "prompt-z", serde_json::json!([])),
            120,
        );
        assert_eq!(other.subagents.len(), 1);
    }

    #[test]
    fn untrusted_subagent_fields_are_bounded_before_they_are_saved() {
        let mut cursor = ClaudeTurnCursor::default();
        open_turn(&mut cursor, 100);
        let start = |agent: &str, kind: &str| {
            subagent_hook(
                "SubagentStart",
                "session-a",
                Some("prompt-a"),
                agent,
                Some(kind),
            )
        };
        // Unusable ids are not stored; an unusable kind only loses its name.
        for bad in [
            "",
            "has space",
            "slash/id",
            &"x".repeat(MAX_SUBAGENT_ID_BYTES + 1),
        ] {
            cursor.observe_at(&start(bad, "Plan"), 110);
        }
        assert!(cursor.subagents.is_empty());
        for (index, kind) in ["", "two words", "../path", &"k".repeat(MAX_KIND_BYTES + 1)]
            .iter()
            .enumerate()
        {
            cursor.observe_at(&start(&format!("agent-{index}"), kind), 110);
        }
        assert_eq!(cursor.subagents.len(), 4);
        assert!(cursor.subagents.iter().all(|s| s.kind.is_none()));
        // Fields of another type are tolerated, not an error.
        let odd: ClaudeHookInput = serde_json::from_value(serde_json::json!({
            "hook_event_name":"SubagentStart","session_id":"session-a","prompt_id":"prompt-a",
            "agent_id":12,"agent_type":["Plan"]
        }))
        .unwrap();
        cursor.observe_at(&odd, 111);
        assert_eq!(cursor.subagents.len(), 4);
        // However many start, the cursor stays far below the size limit.
        for index in 0..100 {
            cursor.observe_at(
                &start(
                    &format!("{index:0>width$}", width = MAX_SUBAGENT_ID_BYTES),
                    "k",
                ),
                112 + index,
            );
        }
        assert_eq!(cursor.subagents.len(), MAX_SUBAGENTS);
        assert_eq!(cursor.subagents.last().unwrap().started_unix, 211);
        assert!(serde_json::to_vec(&cursor).unwrap().len() < MAX_CURSOR_BYTES as usize / 2);
    }

    #[test]
    fn a_cursor_from_an_older_build_still_reads_and_still_schedules() {
        let fixture = Fixture::new();
        let dir = fixture.home.join("agent-hooks/claude");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.json", fixture.shell_id));
        fs::write(
            &path,
            r#"{"session_id":"session-a","turn_id":"prompt-a","completed":true}"#,
        )
        .unwrap();
        let state = claude_state(&fixture.home, &fixture.shell_id, 5_000).unwrap();
        assert_eq!(
            state,
            AgentState {
                activity: AgentActivity::Done,
                since_unix: None,
                subagents: Subagents::default(),
            }
        );
        assert_eq!(
            schedule_state(&fixture.home, &fixture.shell_id),
            Some(("session-a".into(), Some("prompt-a".into())))
        );
        // A newer hook upgrades the file, and an older reader of it (the
        // scheduler's, which knows no new field) is not disturbed by them.
        fixture.record("UserPromptSubmit", "prompt-b").unwrap();
        record_claude_hook(
            &fixture.home,
            &fixture.shell_id,
            &serde_json::json!({
                "hook_event_name":"SubagentStart","session_id":"session-a",
                "prompt_id":"prompt-b","agent_id":"agent-1","agent_type":"Plan"
            })
            .to_string(),
        )
        .unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["subagents"][0]["id"], "agent-1");
        assert_eq!(saved["subagents"][0]["kind"], "Plan");
        assert!(saved["since_unix"].as_u64().unwrap() > 0);
        assert_eq!(
            schedule_state(&fixture.home, &fixture.shell_id),
            Some(("session-a".into(), None))
        );
        let state = claude_state(&fixture.home, &fixture.shell_id, unix_now()).unwrap();
        assert_eq!(state.activity, AgentActivity::Working);
        assert_eq!(state.subagents.working, 1);
        assert!(state.since_unix.is_some());
    }

    #[test]
    fn subagent_hooks_keep_identifiers_only_and_the_activity_follows_the_files() {
        let fixture = Fixture::new();
        assert!(claude_state(&fixture.home, &fixture.shell_id, unix_now()).is_none());
        let write = |payload: serde_json::Value| {
            record_claude_hook(&fixture.home, &fixture.shell_id, &payload.to_string()).unwrap()
        };
        let state = || claude_state(&fixture.home, &fixture.shell_id, unix_now()).unwrap();
        write(
            serde_json::json!({"hook_event_name":"SessionStart","session_id":"session-a","source":"startup"}),
        );
        assert_eq!(state().activity, AgentActivity::Waiting);
        fixture.record("UserPromptSubmit", "prompt-a").unwrap();
        assert_eq!(state().activity, AgentActivity::Working);
        write(serde_json::json!({
            "hook_event_name":"SubagentStart","session_id":"session-a","prompt_id":"prompt-a",
            "agent_id":"agent-1","agent_type":"general-purpose",
            "transcript_path":"/private/transcript.jsonl","cwd":"/private/project"
        }));
        write(serde_json::json!({
            "hook_event_name":"SubagentStop","session_id":"session-a","prompt_id":"prompt-a",
            "agent_id":"agent-1","agent_type":"general-purpose","last_assistant_message":"private subagent reply",
            "agent_transcript_path":"/private/agent.jsonl",
            "background_tasks":[{"id":"agent-1","type":"subagent","status":"running","description":"private task"}]
        }));
        assert_eq!(state().subagents.working, 0);
        write(serde_json::json!({
            "hook_event_name":"SubagentStart","session_id":"session-a","prompt_id":"prompt-a",
            "agent_id":"agent-2","agent_type":"Explore"
        }));
        let working = state();
        assert_eq!(
            (working.activity, working.subagents.working),
            (AgentActivity::Working, 1)
        );
        write(serde_json::json!({
            "hook_event_name":"Stop","session_id":"session-a","prompt_id":"prompt-a",
            "last_assistant_message":"private reply","background_tasks":[],"session_crons":[]
        }));
        let done = state();
        assert_eq!(
            (done.activity, done.subagents.working),
            (AgentActivity::Done, 0)
        );
        let path = fixture
            .home
            .join("agent-hooks/claude")
            .join(format!("{}.json", fixture.shell_id));
        let saved = fs::read_to_string(path).unwrap();
        for private in ["private", "transcript", "/project"] {
            assert!(!saved.contains(private), "{private}: {saved}");
        }
    }

    #[test]
    fn an_open_turn_that_goes_quiet_reads_as_waiting_unless_the_status_line_is_alive() {
        let mut cursor = ClaudeTurnCursor::default();
        open_turn(&mut cursor, 1_000);
        let state = |now: u64, status_line: Option<u64>| cursor.state(now, status_line);
        // Claude is working: the turn is open and was heard from a moment ago.
        assert_eq!(state(1_005, None).activity, AgentActivity::Working);
        assert_eq!(state(1_005, None).since_unix, Some(1_000));
        // The boundary: ten minutes of silence is still working, one second more is not.
        assert_eq!(
            state(1_000 + TURN_QUIET_SECS, None).activity,
            AgentActivity::Working
        );
        let quiet = state(1_000 + TURN_QUIET_SECS + 1, None);
        assert_eq!(quiet.activity, AgentActivity::Waiting);
        assert_eq!(
            quiet.since_unix,
            Some(1_000),
            "waiting since the last sign of life"
        );
        // A turn Esc interrupted sends nothing, ever.
        assert_eq!(
            state(1_000 + 6 * 3_600, None).activity,
            AgentActivity::Waiting
        );
        // Claude drawing a message (the status line ran) is a sign of life, so a
        // long turn that never speaks to a hook keeps working.
        let alive = state(1_000 + 3_600, Some(1_000 + 3_600 - 30));
        assert_eq!(alive.activity, AgentActivity::Working);
        assert_eq!(alive.since_unix, Some(1_000));
        // A status line that stopped long ago is no better than no sign.
        assert_eq!(
            state(1_000 + 3_600, Some(1_100)).activity,
            AgentActivity::Waiting
        );
        // A finished turn is done however long ago, and a cursor from a build that
        // kept no times never counts as quiet.
        cursor.observe_at(&hook("Stop", "session-a", Some("prompt-a")), 1_100);
        assert_eq!(
            cursor.state(1_100 + 24 * 3_600, None).activity,
            AgentActivity::Done
        );
        let old: ClaudeTurnCursor = serde_json::from_str(
            r#"{"session_id":"session-a","turn_id":"prompt-a","completed":false}"#,
        )
        .unwrap();
        assert_eq!(old.state(9_999_999, None).activity, AgentActivity::Working);
    }

    #[test]
    fn a_stop_that_lists_background_work_pauses_the_turn_until_it_wakes() {
        let mut cursor = ClaudeTurnCursor::default();
        open_turn(&mut cursor, 100);
        // A background shell command keeps the turn open (no completion), but
        // Claude is back at its prompt: waiting, not working.
        let stop = stop_listing(
            "session-a",
            "prompt-a",
            serde_json::json!([{"id":"shell-1","type":"shell","status":"running","description":"private"}]),
        );
        assert!(cursor.observe_at(&stop, 200).is_none());
        let paused = cursor.state(201, None);
        assert_eq!(paused.activity, AgentActivity::Waiting);
        assert_eq!(paused.since_unix, Some(200));
        // A subagent in the list is work in progress: working, and counted.
        let stop = stop_listing(
            "session-a",
            "prompt-a",
            serde_json::json!([{"id":"agent-1","type":"subagent","status":"running","agent_type":"Plan"}]),
        );
        cursor.observe_at(&stop, 210);
        let state = cursor.state(211, None);
        assert_eq!(state.activity, AgentActivity::Working);
        assert_eq!(state.subagents.working, 1);
        assert_eq!(state.since_unix, Some(210));
        // The subagent ends: the turn is paused again until Claude is woken.
        cursor.observe_at(
            &subagent_hook(
                "SubagentStop",
                "session-a",
                Some("prompt-a"),
                "agent-1",
                Some("Plan"),
            ),
            300,
        );
        assert_eq!(cursor.state(301, None).activity, AgentActivity::Waiting);
        // The wake-up is a prompt of its own: a new turn, working, then done.
        cursor.observe_at(
            &hook("UserPromptSubmit", "session-a", Some("prompt-b")),
            301,
        );
        assert_eq!(cursor.state(302, None).activity, AgentActivity::Working);
        assert!(
            cursor
                .observe_at(&hook("Stop", "session-a", Some("prompt-b")), 310)
                .is_some()
        );
        assert_eq!(cursor.state(311, None).activity, AgentActivity::Done);
        // A subagent resumed under a paused turn makes it work again.
        let mut resumed = ClaudeTurnCursor::default();
        open_turn(&mut resumed, 100);
        resumed.observe_at(
            &stop_listing(
                "session-a",
                "prompt-a",
                serde_json::json!([{"id":"s","type":"shell"}]),
            ),
            120,
        );
        assert_eq!(resumed.paused_unix, 120);
        resumed.observe_at(
            &subagent_hook(
                "SubagentStart",
                "session-a",
                Some("prompt-a"),
                "agent-9",
                None,
            ),
            150,
        );
        assert_eq!(resumed.paused_unix, 0);
        assert_eq!(resumed.state(151, None).activity, AgentActivity::Working);
        // A Stop of an older turn that lists work does not pause the current one.
        let mut current = ClaudeTurnCursor::default();
        open_turn(&mut current, 100);
        current.observe_at(
            &stop_listing(
                "session-a",
                "prompt-old",
                serde_json::json!([{"id":"s","type":"shell"}]),
            ),
            120,
        );
        assert_eq!(current.paused_unix, 0);
    }

    #[test]
    fn the_recorded_flow_of_an_interactive_background_subagent_reads_right_at_each_step() {
        // The order of events Claude Code 2.1.288 sent for one backgrounded subagent in
        // the terminal: the Stop comes while the subagent still runs, and the subagent's
        // end is followed by a prompt of its own that Claude answers.
        let mut cursor = ClaudeTurnCursor::default();
        let at = |cursor: &ClaudeTurnCursor, now| {
            let state = cursor.state(now, None);
            (state.activity, state.subagents.working)
        };
        cursor.observe_at(&session_start("session-a", "startup"), 0);
        assert_eq!(at(&cursor, 1), (AgentActivity::Waiting, 0));
        open_turn(&mut cursor, 5);
        assert_eq!(at(&cursor, 6), (AgentActivity::Working, 0));
        cursor.observe_at(
            &subagent_hook(
                "SubagentStart",
                "session-a",
                Some("prompt-a"),
                "a591",
                Some("general-purpose"),
            ),
            10,
        );
        assert_eq!(at(&cursor, 11), (AgentActivity::Working, 1));
        cursor.observe_at(
            &stop_listing(
                "session-a",
                "prompt-a",
                serde_json::json!([{"id":"a591","type":"subagent","status":"running","agent_type":"general-purpose","description":"x"}]),
            ),
            12,
        );
        assert_eq!(
            at(&cursor, 13),
            (AgentActivity::Working, 1),
            "the subagent is still running"
        );
        assert_eq!(at(&cursor, 100), (AgentActivity::Working, 1));
        cursor.observe_at(
            &subagent_hook(
                "SubagentStop",
                "session-a",
                Some("prompt-a"),
                "a591",
                Some("general-purpose"),
            ),
            105,
        );
        cursor.observe_at(
            &hook("UserPromptSubmit", "session-a", Some("prompt-b")),
            105,
        );
        assert_eq!(at(&cursor, 106), (AgentActivity::Working, 0));
        cursor.observe_at(
            &stop_listing("session-a", "prompt-b", serde_json::json!([])),
            107,
        );
        assert_eq!(at(&cursor, 108), (AgentActivity::Done, 0));
        // Esc during a foreground subagent: it starts and nothing else ever arrives.
        let mut interrupted = ClaudeTurnCursor::default();
        interrupted.observe_at(
            &hook("UserPromptSubmit", "session-a", Some("prompt-c")),
            1_000,
        );
        interrupted.observe_at(
            &subagent_hook(
                "SubagentStart",
                "session-a",
                Some("prompt-c"),
                "a6cd",
                Some("general-purpose"),
            ),
            1_004,
        );
        assert_eq!(at(&interrupted, 1_010), (AgentActivity::Working, 1));
        let later = 1_004 + SUBAGENT_STALE_SECS;
        assert_eq!(at(&interrupted, later), (AgentActivity::Waiting, 0));
    }

    #[test]
    fn the_stray_subagent_stop_after_a_reply_leaves_the_turn_completed_for_the_scheduler() {
        // After some replies, tool-using or not, Claude 2.1.288 sends a SubagentStop
        // that had no SubagentStart (empty `agent_type`), 3 to 5 seconds after the Stop,
        // sometimes two of them, each carrying the finished turn's own prompt.
        let mut cursor = ClaudeTurnCursor::default();
        open_turn(&mut cursor, 100);
        assert!(
            cursor
                .observe_at(&hook("Stop", "session-a", Some("prompt-a")), 150)
                .is_some()
        );
        let suggestion = subagent_hook(
            "SubagentStop",
            "session-a",
            Some("prompt-a"),
            "suggest",
            Some(""),
        );
        assert!(cursor.observe_at(&suggestion, 154).is_none());
        let second = subagent_hook(
            "SubagentStop",
            "session-a",
            Some("prompt-a"),
            "suggest-too",
            Some(""),
        );
        assert!(cursor.observe_at(&second, 156).is_none());
        // The scheduler's view and the display's are one cursor: still completed.
        assert!(cursor.completed);
        assert_eq!(completed(&cursor), ("session-a".into(), true));
        let state = cursor.state(154 + 3_600, None);
        assert_eq!(
            (state.activity, state.since_unix),
            (AgentActivity::Done, Some(150))
        );
        // A real pair under an open turn the cursor never saw begin (its
        // UserPromptSubmit was missed) does withdraw it, from the start on.
        let start = subagent_hook(
            "SubagentStart",
            "session-a",
            Some("prompt-new"),
            "agent-1",
            Some("Plan"),
        );
        cursor.observe_at(&start, 200);
        assert!(!cursor.completed);
        assert_eq!(cursor.state(201, None).activity, AgentActivity::Working);
        let stop = subagent_hook(
            "SubagentStop",
            "session-a",
            Some("prompt-new"),
            "agent-1",
            Some("Plan"),
        );
        cursor.observe_at(&stop, 210);
        assert!(!cursor.completed, "a pair never completes a turn");
        // A new real turn ends the same way, and the suggestion after it changes nothing.
        cursor.observe_at(
            &hook("UserPromptSubmit", "session-a", Some("prompt-b")),
            300,
        );
        assert_eq!(cursor.state(301, None).activity, AgentActivity::Working);
        cursor.observe_at(&hook("Stop", "session-a", Some("prompt-b")), 320);
        cursor.observe_at(
            &subagent_hook(
                "SubagentStop",
                "session-a",
                Some("prompt-b"),
                "suggest-2",
                Some(""),
            ),
            324,
        );
        assert!(cursor.completed);
        assert_eq!(cursor.state(325, None).activity, AgentActivity::Done);
    }

    #[test]
    fn a_paired_subagent_stop_in_an_open_turn_keeps_it_open_and_old_cursors_still_read() {
        // A real subagent inside a turn: its start and stop leave the turn open.
        let mut cursor = ClaudeTurnCursor::default();
        open_turn(&mut cursor, 100);
        for event in ["SubagentStart", "SubagentStop"] {
            cursor.observe_at(
                &subagent_hook(
                    event,
                    "session-a",
                    Some("prompt-a"),
                    "agent-1",
                    Some("Plan"),
                ),
                110,
            );
        }
        assert_eq!(completed(&cursor), ("session-a".into(), false));
        assert!(cursor.subagents.is_empty());
        // The turn's Stop then completes it once.
        assert!(
            cursor
                .observe_at(&hook("Stop", "session-a", Some("prompt-a")), 120)
                .is_some()
        );
        assert_eq!(completed(&cursor), ("session-a".into(), true));
        // A cursor written by the previous build (which kept an `ended` flag) reads.
        let old: ClaudeTurnCursor = serde_json::from_str(
            r#"{"session_id":"session-a","turn_id":"prompt-a","completed":true,"ended":true,"since_unix":9}"#,
        )
        .unwrap();
        assert_eq!(old.state(10, None).activity, AgentActivity::Done);
    }
}
