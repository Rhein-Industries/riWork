//! The global and the project orchestrators, whichever way they run.
//!
//! An orchestrator is a terminal (a Codex in a tmux session, `sessions`) or a
//! chat (a Codex or Claude conversation of the chat host, `chat`), as the
//! Settings choice was when it was created. Everything that addresses one (the
//! CLI, the MCP tools, the scheduler, the windows) asks this module for "the
//! orchestrator of this scope" and gets whichever exists, so a scope has at most
//! one live orchestrator across both: creating one when the other kind already
//! exists returns that one.
//!
//! **A chat orchestrator** is an ordinary chat that the host knows by its
//! `orchestrator` scope (`ChatInfo`). Its id is the chat's id, which is also the
//! `id` of its entry in `riwork orchestrator list --json`, so one UUID names it
//! everywhere (`chat_id` repeats it for readers that look for the chat). It
//! works in the same folder as a terminal orchestrator
//! (`RIWORK_HOME/orchestrator`, `RIWORK_HOME/orchestrators/projects/ID`), with
//! the same skill installed there, and starts with the same message. A project
//! orchestrator never asks before it acts (`Full`, as its terminal launches
//! with approvals and sandbox off); the global one asks before commands and
//! edits (`Supervised`).
//!
//! **Finding them** asks the chat host when it runs and otherwise reads the
//! chats' saved info, so looking never starts a host. Acting on one (sending,
//! closing, creating) starts it. Closing a chat orchestrator deletes its chat,
//! as closing a terminal orchestrator ends its session: the next one starts
//! afresh, in whatever mode the setting then names.

use crate::{
    activity::AgentActivity,
    chat::{
        self,
        client::{Client, socket_path},
        model::{
            ApprovalMode, ChatCommand, ChatInfo, ChatState, NewChat, ORCHESTRATOR_EXISTS,
            OrchestratorScope, Provider,
        },
    },
    sessions::{
        SessionManager, ShellSession, orchestrator_context, orchestrator_prompt,
        orchestrator_skill_version,
    },
    settings::OrchestratorRuns,
};
use serde_json::{Value, json};
use std::{
    env, fs,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use uuid::Uuid;

/// How orchestrator code reaches the chat host.
#[derive(Clone, Copy)]
pub struct ChatHost<'a> {
    /// The RiWork data directory the host serves.
    pub home: &'a Path,
    /// Makes sure a host runs and returns its socket: `system_ensure`, or, in a
    /// test, the socket of the in-process host.
    pub ensure: &'a dyn Fn(&Path) -> Result<PathBuf, String>,
}

impl ChatHost<'_> {
    /// A connection to the host, which is started if it is not running.
    pub fn connect(&self) -> Result<Client, String> {
        Client::connect(&(self.ensure)(self.home)?)
    }

    /// A connection to the host if one runs; looking at chats starts none.
    fn connect_running(&self) -> Option<Client> {
        Client::connect(&socket_path(self.home)).ok()
    }
}

/// The real way to reach the chat host of `home`: the one that runs, else one
/// started from this executable. A test build never starts one.
pub fn system_ensure(home: &Path) -> Result<PathBuf, String> {
    let socket = socket_path(home);
    if UnixStream::connect(&socket).is_ok() {
        return Ok(socket);
    }
    if cfg!(test) {
        return Err("a test build never starts a chat host from its own executable".into());
    }
    let executable = env::current_exe()
        .map_err(|error| format!("Cannot locate the RiWork executable: {error}"))?;
    chat::host::ensure(home, &executable)
}

/// An orchestrator and how it runs.
#[derive(Clone, Debug, PartialEq)]
pub enum Orchestrator {
    Terminal(ShellSession),
    Chat(ChatInfo),
}

/// The scope of the orchestrator of `project_id`, or of the global one.
pub fn scope_of(project_id: Option<&str>) -> OrchestratorScope {
    match project_id {
        Some(project_id) => OrchestratorScope::Project {
            project_id: project_id.to_owned(),
        },
        None => OrchestratorScope::Global,
    }
}

/// What the tab of an orchestrator says it is, in either mode.
pub fn tab_title(scope: &OrchestratorScope) -> &'static str {
    match scope {
        OrchestratorScope::Global => "G·ORCH · GLOBAL",
        OrchestratorScope::Project { .. } => "P·ORCH · PROJECT",
    }
}

/// The chat orchestrators there are, oldest first.
pub fn chat_orchestrators(host: &ChatHost) -> Vec<ChatInfo> {
    running_chat_orchestrators(host).unwrap_or_else(|| saved_chat_orchestrators(host))
}

/// The chat orchestrators the host knows, if one runs.
pub fn running_chat_orchestrators(host: &ChatHost) -> Option<Vec<ChatInfo>> {
    let mut chats = host.connect_running()?.list().ok()?;
    chats.retain(|chat| chat.orchestrator.is_some());
    Some(chats)
}

/// The chat orchestrators the saved chats name, for when no host runs: every one is
/// stopped, as the next host will find it.
pub fn saved_chat_orchestrators(host: &ChatHost) -> Vec<ChatInfo> {
    let mut chats = chat::log::read_infos(host.home);
    chats.retain(|chat| chat.orchestrator.is_some());
    chats
}

fn terminal_of(
    sessions: &SessionManager,
    scope: &OrchestratorScope,
) -> Result<Option<ShellSession>, String> {
    match scope {
        OrchestratorScope::Global => sessions.orchestrator_get(),
        OrchestratorScope::Project { project_id } => {
            sessions.orchestrator_get_for_project(project_id)
        }
    }
}

/// The orchestrator of `scope`, if there is one. A terminal whose session is
/// still there wins over a chat (there should never be both); a terminal whose
/// session is gone is reported as it always was, unless a chat took the scope.
pub fn find(
    sessions: &SessionManager,
    host: &ChatHost,
    scope: &OrchestratorScope,
) -> Result<Option<Orchestrator>, String> {
    let terminal = terminal_of(sessions, scope)?;
    if let Some(shell) = terminal.as_ref().filter(|shell| shell.alive) {
        return Ok(Some(Orchestrator::Terminal(shell.clone())));
    }
    let chat = chat_orchestrators(host)
        .into_iter()
        .find(|chat| chat.orchestrator.as_ref() == Some(scope));
    Ok(match (chat, terminal) {
        (Some(chat), _) => Some(Orchestrator::Chat(chat)),
        (None, Some(shell)) => Some(Orchestrator::Terminal(shell)),
        (None, None) => None,
    })
}

/// The orchestrator of `scope`, created as `runs` says when there is none, and
/// whether it was made now. One that exists is returned as it is, whatever it
/// runs as. A custom `command` is a terminal's whatever the setting says.
/// `project_root` is the project's root (a project orchestrator needs it); `cwd`
/// is where a global orchestrator's custom command runs.
pub fn create(
    sessions: &SessionManager,
    host: &ChatHost,
    scope: &OrchestratorScope,
    project_root: Option<PathBuf>,
    cwd: PathBuf,
    command: Option<String>,
    runs: OrchestratorRuns,
) -> Result<(Orchestrator, bool), String> {
    let chat_wanted = match runs {
        OrchestratorRuns::Chat(provider) if command.is_none() => Some(provider),
        _ => None,
    };
    if let Some(existing) = find(sessions, host, scope)?
        && match &existing {
            Orchestrator::Terminal(shell) => shell.alive,
            Orchestrator::Chat(_) => true,
        }
    {
        return Ok((existing, false));
    }
    if let Some(provider) = chat_wanted {
        // A terminal that is gone makes room for the chat.
        sessions.forget_dead_orchestrator(scope.project_id())?;
        return create_chat(sessions, host, scope, project_root.as_deref(), provider)
            .map(|(chat, created)| (Orchestrator::Chat(chat), created));
    }
    match scope {
        OrchestratorScope::Global => sessions.orchestrator_create(cwd, command),
        OrchestratorScope::Project { project_id } => sessions.orchestrator_create_for_project(
            project_id.clone(),
            project_root.ok_or("a project orchestrator needs its project's root")?,
            command,
        ),
    }
    .map(|shell| (Orchestrator::Terminal(shell), true))
}

/// How much a chat orchestrator may do on its own: a project's never asks, as
/// its terminal launches with approvals and sandbox off, and the global one asks
/// before commands and edits.
pub fn approval_mode(scope: &OrchestratorScope) -> ApprovalMode {
    match scope {
        OrchestratorScope::Global => ApprovalMode::Supervised,
        OrchestratorScope::Project { .. } => ApprovalMode::Full,
    }
}

/// Starts the scope's orchestrator as a chat and gives it its first message,
/// the one a terminal orchestrator is started with. If the scope turns out to
/// have a chat already (another process was quicker), that one is returned, not
/// created, and nothing is sent. A chat that cannot be started or told its first
/// message is deleted again, so an orchestrator that exists is one that was
/// started.
fn create_chat(
    sessions: &SessionManager,
    host: &ChatHost,
    scope: &OrchestratorScope,
    project_root: Option<&Path>,
    provider: Provider,
) -> Result<(ChatInfo, bool), String> {
    let project_id = scope.project_id();
    let skill = sessions.orchestrator_skill_path_scoped(project_id)?;
    let executable =
        env::current_exe().map_err(|error| format!("resolve RiWork executable: {error}"))?;
    let root = project_root
        .map(|root| {
            root.canonicalize()
                .map_err(|error| format!("resolve {}: {error}", root.display()))
        })
        .transpose()?;
    let first = orchestrator_prompt(&skill, &executable, project_id, root.as_deref(), false);
    let mut client = host.connect()?;
    let created = client.create(NewChat {
        provider,
        project_id: project_id.map(str::to_owned),
        worktree_id: None,
        cwd: orchestrator_context(host.home, project_id),
        title: Some(tab_title(scope).to_owned()),
        approval_mode: approval_mode(scope),
        model: None,
        effort: None,
        codex_account_id: None,
        orchestrator: Some(scope.clone()),
    });
    let info = match created {
        Ok(info) => info,
        Err(error) => {
            if let Some(existing) = error.strip_prefix(ORCHESTRATOR_EXISTS) {
                let existing = existing.trim();
                return client
                    .list()?
                    .into_iter()
                    .find(|chat| chat.id == existing)
                    .map(|chat| (chat, false))
                    .ok_or(error);
            }
            return Err(error);
        }
    };
    let started = match &info.state {
        ChatState::Failed { message } => Err(message.clone()),
        _ => client.command(&info.id, ChatCommand::Send { text: first }),
    };
    if let Err(error) = started {
        let _ = client.delete(&info.id);
        return Err(format!("The orchestrator chat could not start: {error}"));
    }
    record_chat_skill(host.home, &info)?;
    Ok((info, true))
}

/// Sends `text` to a chat orchestrator as a message.
pub fn send(host: &ChatHost, chat: &ChatInfo, text: &str) -> Result<(), String> {
    host.connect()?.command(
        &chat.id,
        ChatCommand::Send {
            text: text.to_owned(),
        },
    )
}

/// The last `lines` lines of a chat orchestrator's conversation as plain text.
pub fn output(home: &Path, chat: &ChatInfo, lines: usize) -> Result<Vec<String>, String> {
    let transcript = chat::log::read_transcript(home, &chat.id)?;
    Ok(chat::text::tail(&transcript, lines))
}

/// Ends a chat orchestrator: its agent stops and its chat is deleted.
pub fn close(host: &ChatHost, chat: &ChatInfo) -> Result<(), String> {
    host.connect()?.delete(&chat.id)?;
    forget_chat_skill(host.home, chat);
    Ok(())
}

/// LOAD SKILL for a chat orchestrator: the installed skill is refreshed and its
/// full text goes to the chat as a message. Nothing happens when the chat has
/// the current skill already. The chat must be ready for a message.
pub fn load_chat_skill(
    sessions: &SessionManager,
    host: &ChatHost,
    chat: &ChatInfo,
    project_root: Option<&Path>,
) -> Result<ChatInfo, String> {
    if chat_skill_is_current(host.home, chat) {
        return Ok(chat.clone());
    }
    // The state the caller holds may be old: ask the host.
    let mut client = host.connect()?;
    let chat = client
        .list()?
        .into_iter()
        .find(|listed| listed.id == chat.id)
        .ok_or_else(|| format!("unknown chat {}", chat.id))?;
    if !matches!(chat.state, ChatState::Idle | ChatState::Stopped) {
        return Err("the orchestrator is busy; load the skill when it is idle".to_owned());
    }
    let project_id = chat.project_id.as_deref();
    let skill = sessions.orchestrator_skill_path_scoped(project_id)?;
    let executable =
        env::current_exe().map_err(|error| format!("resolve RiWork executable: {error}"))?;
    let message = orchestrator_prompt(&skill, &executable, project_id, project_root, true);
    client.command(&chat.id, ChatCommand::Send { text: message })?;
    record_chat_skill(host.home, &chat)?;
    Ok(chat)
}

// ---- The skill a chat has been given ---------------------------------------------------

/// What `chat-skill.json` in the orchestrator's folder says: the skill version
/// the chat was last given. A terminal orchestrator keeps this in its session.
#[derive(serde::Serialize, serde::Deserialize)]
struct SkillMark {
    chat_id: String,
    version: String,
}

fn skill_mark_path(home: &Path, project_id: Option<&str>) -> PathBuf {
    orchestrator_context(home, project_id).join("chat-skill.json")
}

fn read_skill_mark(home: &Path, chat: &ChatInfo) -> Option<SkillMark> {
    let text = fs::read_to_string(skill_mark_path(home, chat.project_id.as_deref())).ok()?;
    serde_json::from_str::<SkillMark>(&text)
        .ok()
        .filter(|mark| mark.chat_id == chat.id)
}

/// The skill version the chat was given, if it was.
pub fn chat_skill_version(home: &Path, chat: &ChatInfo) -> Option<String> {
    read_skill_mark(home, chat).map(|mark| mark.version)
}

/// Whether the chat has the skill this build ships: what a terminal
/// orchestrator's LOAD SKILL bar checks.
pub fn chat_skill_is_current(home: &Path, chat: &ChatInfo) -> bool {
    chat_skill_version(home, chat).as_deref() == Some(orchestrator_skill_version().as_str())
}

fn record_chat_skill(home: &Path, chat: &ChatInfo) -> Result<(), String> {
    let path = skill_mark_path(home, chat.project_id.as_deref());
    let mark = SkillMark {
        chat_id: chat.id.clone(),
        version: orchestrator_skill_version(),
    };
    let temporary = path.with_file_name(format!(".chat-skill-{}.tmp", Uuid::new_v4()));
    let result = serde_json::to_vec(&mark)
        .map_err(|error| error.to_string())
        .and_then(|bytes| fs::write(&temporary, bytes).map_err(|error| error.to_string()))
        .and_then(|()| fs::rename(&temporary, &path).map_err(|error| error.to_string()));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("Cannot record the orchestrator's skill: {error}"))
}

fn forget_chat_skill(home: &Path, chat: &ChatInfo) {
    if read_skill_mark(home, chat).is_some() {
        let _ = fs::remove_file(skill_mark_path(home, chat.project_id.as_deref()));
    }
}

// ---- How they are shown -------------------------------------------------------------------

/// The agent behind a chat, as the CLI and the phone name it.
pub fn provider_word(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "codex",
        Provider::Claude => "claude",
    }
}

/// A chat's state in a word.
pub fn state_word(state: &ChatState) -> &'static str {
    match state {
        ChatState::Starting => "starting",
        ChatState::Idle => "idle",
        ChatState::Running => "running",
        ChatState::Waiting => "waiting",
        ChatState::Stopped => "stopped",
        ChatState::Failed { .. } => "failed",
    }
}

/// What the agent of a chat orchestrator is doing, as a terminal agent's activity says
/// it. An orchestrator is told its start message at once, so an idle one has finished a
/// turn.
pub fn chat_activity(state: &ChatState) -> AgentActivity {
    match state {
        ChatState::Running => AgentActivity::Working,
        ChatState::Waiting => AgentActivity::Waiting,
        ChatState::Idle => AgentActivity::Done,
        ChatState::Starting | ChatState::Stopped => AgentActivity::Unknown,
        ChatState::Failed { .. } => AgentActivity::Exited,
    }
}

/// An orchestrator as `status --json` and the MCP tools show it: a terminal's
/// session with its `mode`, or a chat's entry.
pub fn entry(home: &Path, orchestrator: &Orchestrator) -> Value {
    match orchestrator {
        Orchestrator::Terminal(shell) => {
            let mut entry = serde_json::to_value(shell).unwrap_or(Value::Null);
            entry["mode"] = json!("terminal");
            entry
        }
        Orchestrator::Chat(chat) => chat_entry(home, chat),
    }
}

/// A chat orchestrator as `riwork orchestrator list --json` and `status --json`
/// show it: the fields of a terminal orchestrator that mean something for a
/// chat, with the three that tell the readers it is one (`mode`, `chat_id`,
/// `provider`) and its state. `id` is the chat's id. `alive` is true for as long
/// as the chat exists: a stopped or failed chat resumes with the next message.
pub fn chat_entry(home: &Path, chat: &ChatInfo) -> Value {
    let provider = provider_word(chat.provider);
    let mut entry = json!({
        "id": chat.id,
        "project_id": chat.project_id,
        "worktree_id": null,
        "kind": "orchestrator",
        "cwd": chat.cwd,
        "command": null,
        "harness": provider,
        "unrestricted": chat.approval_mode == ApprovalMode::Full,
        "codex_account_id": chat.codex_account_id,
        "orchestrator_skill_loaded": chat_skill_version(home, chat).is_some(),
        "orchestrator_skill_version": chat_skill_version(home, chat),
        "created_at_unix": chat.created_at_unix,
        "alive": true,
        "mode": "chat",
        "chat_id": chat.id,
        "provider": provider,
        "title": chat.title,
        "state": state_word(&chat.state),
        "activity": chat_activity(&chat.state).as_str(),
    });
    let fields = entry.as_object_mut().expect("an object");
    if let ChatState::Failed { message } = &chat.state {
        fields.insert("state_message".into(), json!(message));
    }
    if let Some(at) = last_activity(home, &chat.id) {
        fields.insert("last_activity_unix".into(), json!(at));
    }
    entry
}

/// What a chat orchestrator adds to its project's row in `riwork project list --json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFact {
    pub project_id: String,
    pub activity: AgentActivity,
    pub last_activity_unix: Option<u64>,
}

/// The facts of the chat orchestrators that belong to a project (the global one belongs
/// to none), as a terminal orchestrator's shell counts for its project.
pub fn project_facts(home: &Path, chats: &[ChatInfo]) -> Vec<ProjectFact> {
    chats
        .iter()
        .filter_map(|chat| {
            Some(ProjectFact {
                project_id: chat.project_id.clone()?,
                activity: chat_activity(&chat.state),
                last_activity_unix: last_activity(home, &chat.id),
            })
        })
        .collect()
}

/// When the chat's log last grew, in Unix seconds.
fn last_activity(home: &Path, chat_id: &str) -> Option<u64> {
    let path = chat::log::chat_dir(home, chat_id)?.join("events.jsonl");
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// A chat orchestrator on one line, as `print_shell` shows a terminal's.
pub fn chat_line(chat: &ChatInfo) -> String {
    let scope = match &chat.project_id {
        Some(id) => format!("project {id}"),
        None => "orchestrator".to_owned(),
    };
    format!(
        "{}  Orchestrator  {}  {}  {}  chat {}\n",
        chat.id,
        state_word(&chat.state),
        scope,
        chat.cwd.display(),
        provider_word(chat.provider)
    )
}

#[cfg(test)]
mod tests;
