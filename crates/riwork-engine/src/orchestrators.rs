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
//! **How a new one runs** is `plan`'s answer for its scope: a project's own choice
//! (Project Settings), else Settings > Agent sessions. The default, `Automatic`,
//! starts a Muse chat on Wonder Full and, when Muse cannot start, a Codex chat on
//! GPT-6.1-Sol, and says so (`Created::fallback`); an explicit chat that cannot
//! start is an error, never replaced by another. The desktop, the CLI (and the
//! phone through it) and the MCP tools all create through `plan` and `create`.
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
    delegation::{self, Delegation},
    sessions::{
        SessionManager, ShellSession, orchestrator_context, orchestrator_delegation_version,
        orchestrator_prompt, orchestrator_skill_version, orchestrator_told,
    },
    settings::{
        AUTOMATIC_ORCHESTRATORS, OrchestratorRuns, Settings, SettingsStore, provider_label,
    },
    store::{Project, ProjectOrchestrator, Store},
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    env, fs,
    os::unix::{fs::OpenOptionsExt, net::UnixStream},
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

/// An orchestrator chat's name: fixed, so that no message renames it; only an explicit
/// rename does.
pub fn chat_title(scope: &OrchestratorScope) -> &'static str {
    match scope {
        OrchestratorScope::Global => "Global orchestrator",
        OrchestratorScope::Project { .. } => "Project orchestrator",
    }
}

/// The fixed name behind any title an orchestrator chat was given by RiWork rather than
/// by the user: its tab label or its name.
pub fn fixed_title(title: &str) -> Option<&'static str> {
    [
        OrchestratorScope::Global,
        OrchestratorScope::Project {
            project_id: String::new(),
        },
    ]
    .iter()
    .find(|scope| title == tab_title(scope) || title == chat_title(scope))
    .map(chat_title)
}

/// A tab's title as the tab strip shows it: an orchestrator's in sentence case under Native
/// ("G·Orch · Global", as Native's status bar says G·Orch), any other title as it is.
/// The desktop supplies its current typography policy; the engine has no UI state.
pub fn shown_tab_title_with(title: &str, quiet: impl FnOnce(&str) -> String) -> String {
    let scopes = [
        OrchestratorScope::Global,
        OrchestratorScope::Project {
            project_id: String::new(),
        },
    ];
    if scopes.iter().any(|scope| tab_title(scope) == title) {
        quiet(title)
    } else {
        title.to_owned()
    }
}

/// The chat orchestrators there are, oldest first.
pub fn chat_orchestrators(host: &ChatHost) -> Vec<ChatInfo> {
    running_chat_orchestrators(host).unwrap_or_else(|| saved_chat_orchestrators(host))
}

/// All shared chats the host knows, without starting a host just to list them.
pub fn running_chats(host: &ChatHost) -> Option<Vec<ChatInfo>> {
    host.connect_running()?.list().ok()
}

/// The chat orchestrators the host knows, if one runs.
pub fn running_chat_orchestrators(host: &ChatHost) -> Option<Vec<ChatInfo>> {
    let mut chats = running_chats(host)?;
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

/// Who decided how a new orchestrator runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanSource {
    /// Settings > Agent sessions.
    Settings,
    /// The project's own choice in Project Settings.
    Project,
    /// The command that creates it (`riwork orchestrator create --mode …`).
    Request,
}

impl PlanSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Project => "project",
            Self::Request => "request",
        }
    }
}

/// How a new orchestrator of a scope runs, and who said so.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub runs: OrchestratorRuns,
    pub source: PlanSource,
    /// Saved choices this build could not read and passed over, in words.
    pub notes: Vec<String>,
}

/// How a new orchestrator of `project`'s scope (none: the global one) runs: the
/// project's own choice, if it makes one, else Settings. Only that project's
/// orchestrator follows its choice; the global one always follows Settings.
pub fn plan_for(settings: &Settings, project: Option<&Project>) -> Plan {
    let mut notes = Vec::new();
    let orchestrator = project.map(|project| &project.orchestrator);
    if let Some(ProjectOrchestrator::Choose(runs)) = orchestrator {
        return Plan {
            runs: runs.clone(),
            source: PlanSource::Project,
            notes,
        };
    }
    if let Some(note) = orchestrator.and_then(ProjectOrchestrator::unreadable_note) {
        notes.push(note);
    }
    if let Some(note) = settings.orchestrator_unreadable_note() {
        notes.push(note);
    }
    Plan {
        runs: settings.orchestrator_runs(),
        source: PlanSource::Settings,
        notes,
    }
}

/// `plan_for` as the files of `home` say now: Settings and, for a project, the
/// store. Read each time, as `settings::orchestrator_runs` is, so no window, CLI or
/// MCP server holds a copy that another could outdate. Unreadable settings read as
/// their defaults; a project that is not in the store is an error.
pub fn plan(home: &Path, project_id: Option<&str>) -> Result<Plan, String> {
    let settings = SettingsStore::open(home)
        .and_then(|store| store.load())
        .unwrap_or_default();
    let project = project_id
        .map(|id| Store::open(home)?.snapshot()?.project(id).cloned())
        .transpose()?;
    Ok(plan_for(&settings, project.as_ref()))
}

/// The delegation policy of `project`'s orchestrator (none: the global one's): the
/// project's own choices, then Settings', then RiWork's, field by field. The global
/// orchestrator follows Settings.
pub fn delegation_for(settings: &Settings, project: Option<&Project>) -> Delegation {
    delegation::resolve(
        &settings.delegation_policy,
        project.map(|project| &project.delegation_policy),
    )
}

/// `delegation_for` as the files of `home` say now, read each time as `plan` reads
/// them. It never fails: unreadable settings read as their defaults, and a project the
/// store cannot name (or a store that cannot be read) adds nothing to Settings.
pub fn delegation(home: &Path, project_id: Option<&str>) -> Delegation {
    let settings = SettingsStore::open(home)
        .and_then(|store| store.load())
        .unwrap_or_default();
    let project = project_id.and_then(|id| {
        Store::open(home)
            .and_then(|store| store.snapshot())
            .ok()?
            .project(id)
            .ok()
            .cloned()
    });
    delegation_for(&settings, project.as_ref())
}

/// What `create` did.
#[derive(Clone, Debug, PartialEq)]
pub struct Created {
    pub orchestrator: Orchestrator,
    /// Made now, or the one there was.
    pub created: bool,
    /// When `Automatic` could not start its first choice: what could not start,
    /// why, and what runs instead. Shown wherever the orchestrator was asked for.
    pub fallback: Option<String>,
}

/// The orchestrator of `scope`, created as `runs` says when there is none, and
/// whether it was made now. One that exists is returned as it is, whatever it
/// runs as: its mode, provider and model change only when it is closed and
/// created again. A custom `command` is a terminal's whatever the setting says.
/// `project_root` is the project's root (a project orchestrator needs it); `cwd`
/// is where a global orchestrator's custom command runs.
///
/// `Automatic` starts the first of `AUTOMATIC_ORCHESTRATORS` whose chat starts,
/// and names the ones that did not in `fallback`; if none starts, the error names
/// each and why. An explicit chat is that chat or an error.
///
/// Creators of a scope take turns, in every process and whichever mode they ask for
/// (`lock_scope`): each looks and, finding none, makes and starts its orchestrator
/// before the next one looks.
pub fn create(
    sessions: &SessionManager,
    host: &ChatHost,
    scope: &OrchestratorScope,
    project_root: Option<PathBuf>,
    cwd: PathBuf,
    command: Option<String>,
    runs: &OrchestratorRuns,
) -> Result<Created, String> {
    let chats: Vec<(Provider, Option<String>)> = match runs {
        _ if command.is_some() => Vec::new(),
        OrchestratorRuns::Automatic => AUTOMATIC_ORCHESTRATORS
            .iter()
            .map(|(provider, model)| (*provider, Some((*model).to_owned())))
            .collect(),
        OrchestratorRuns::Chat { provider, model } => vec![(*provider, model.clone())],
        OrchestratorRuns::Terminal => Vec::new(),
    };
    let _turn = lock_scope(host.home, scope)?;
    if let Some(existing) = find(sessions, host, scope)?
        && match &existing {
            Orchestrator::Terminal(shell) => shell.alive,
            Orchestrator::Chat(_) => true,
        }
    {
        return Ok(Created {
            orchestrator: existing,
            created: false,
            fallback: None,
        });
    }
    if !chats.is_empty() {
        // A terminal that is gone makes room for the chat.
        sessions.forget_dead_orchestrator(scope.project_id())?;
        let automatic = matches!(runs, OrchestratorRuns::Automatic);
        let mut failures = Vec::new();
        for (provider, model) in chats {
            let name = chat_name(provider, model.as_deref());
            match create_chat(
                sessions,
                host,
                scope,
                project_root.as_deref(),
                provider,
                model,
            ) {
                Ok((chat, created)) => {
                    // One another process made meanwhile is not this call's fallback.
                    let fallback = (created && !failures.is_empty())
                        .then(|| format!("{}. It runs as a {name} instead.", failures.join(". ")));
                    return Ok(Created {
                        orchestrator: Orchestrator::Chat(chat),
                        created,
                        fallback,
                    });
                }
                Err(error) if automatic => failures.push(format!(
                    "{name} did not start: {}",
                    error.strip_prefix(CHAT_DID_NOT_START).unwrap_or(&error)
                )),
                Err(error) => return Err(error),
            }
        }
        return Err(format!(
            "No orchestrator could start. {}.",
            failures.join(". ")
        ));
    }
    match scope {
        OrchestratorScope::Global => sessions.orchestrator_create(cwd, command),
        OrchestratorScope::Project { project_id } => sessions.orchestrator_create_for_project(
            project_id.clone(),
            project_root.ok_or("a project orchestrator needs its project's root")?,
            command,
        ),
    }
    .map(|shell| Created {
        orchestrator: Orchestrator::Terminal(shell),
        created: true,
        fallback: None,
    })
}

/// "Muse chat on muse-wonder-full-1.0", "Codex chat on its default model".
fn chat_name(provider: Provider, model: Option<&str>) -> String {
    match model {
        Some(model) => format!("{} chat on {model}", provider_label(provider)),
        None => format!("{} chat on its default model", provider_label(provider)),
    }
}

/// What an orchestrator chat that could not start says first.
const CHAT_DID_NOT_START: &str = "The orchestrator chat could not start: ";

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
/// have a chat already (made by a build that does not take turns), that one is
/// returned, not created, and nothing is sent. A chat that cannot be started, told
/// its first message or have that recorded is deleted again, so an orchestrator
/// that exists is one that was started.
fn create_chat(
    sessions: &SessionManager,
    host: &ChatHost,
    scope: &OrchestratorScope,
    project_root: Option<&Path>,
    provider: Provider,
    model: Option<String>,
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
    let policy = delegation(host.home, project_id);
    let first = orchestrator_prompt(
        &skill,
        &executable,
        project_id,
        root.as_deref(),
        &policy,
        false,
    );
    let mut client = host.connect()?;
    let created = client.create(NewChat {
        parent_id: None,
        provider,
        project_id: project_id.map(str::to_owned),
        worktree_id: None,
        cwd: orchestrator_context(host.home, project_id),
        title: Some(chat_title(scope).to_owned()),
        approval_mode: approval_mode(scope),
        model,
        effort: None,
        codex_account_id: None,
        orchestrator: Some(scope.clone()),
        fast: false,
        // An orchestrator chat starts off, as every Muse RiWork starts unless asked on.
        contributor_mode: crate::chat::model::ContributorMode::Off,
        contributor_opt_out: false,
        claude_billing: None,
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
    }
    .and_then(|()| record_chat_skill(host.home, &info, &policy));
    if let Err(error) = started {
        let _ = client.delete(&info.id);
        return Err(format!("{CHAT_DID_NOT_START}{error}"));
    }
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
/// full text goes to the chat as a message, with its scope's delegation policy now.
/// Nothing happens when the chat has the current skill and policy already. The chat
/// must be ready for a message.
pub fn load_chat_skill(
    sessions: &SessionManager,
    host: &ChatHost,
    chat: &ChatInfo,
    project_root: Option<&Path>,
) -> Result<ChatInfo, String> {
    let policy = delegation(host.home, chat.project_id.as_deref());
    if chat_skill_is_current_with(host.home, chat, &policy) {
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
    let message = orchestrator_prompt(&skill, &executable, project_id, project_root, &policy, true);
    client.command(&chat.id, ChatCommand::Send { text: message })?;
    record_chat_skill(host.home, &chat, &policy)?;
    Ok(chat)
}

// ---- The skill a chat has been given ---------------------------------------------------

/// What `chat-skill.json` in the orchestrator's folder says: the skill version
/// the chat was last given, and the delegation policy with it (none from a build before
/// that was recorded). A terminal orchestrator keeps this in its session.
#[derive(serde::Serialize, serde::Deserialize)]
struct SkillMark {
    chat_id: String,
    version: String,
    #[serde(default)]
    delegation: Option<String>,
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

/// Whether the chat has the skill this build ships and the delegation policy its scope
/// resolves to now: what a terminal orchestrator's LOAD SKILL bar checks.
pub fn chat_skill_is_current(home: &Path, chat: &ChatInfo) -> bool {
    chat_skill_is_current_with(home, chat, &delegation(home, chat.project_id.as_deref()))
}

/// The orchestrator chats among `chats` that are not current (`chat_skill_is_current`),
/// with Settings and the store read once for all of them: what a window polls.
pub fn stale_chat_skills(home: &Path, chats: &[ChatInfo]) -> HashSet<String> {
    let orchestrators = chats
        .iter()
        .filter(|chat| chat.orchestrator.is_some())
        .collect::<Vec<_>>();
    if orchestrators.is_empty() {
        return HashSet::new();
    }
    let settings = SettingsStore::open(home)
        .and_then(|store| store.load())
        .unwrap_or_default();
    let state = Store::open(home).and_then(|store| store.snapshot()).ok();
    orchestrators
        .into_iter()
        .filter(|chat| {
            let project = chat
                .project_id
                .as_deref()
                .and_then(|id| state.as_ref()?.project(id).ok());
            !chat_skill_is_current_with(home, chat, &delegation_for(&settings, project))
        })
        .map(|chat| chat.id.clone())
        .collect()
}

fn chat_skill_is_current_with(home: &Path, chat: &ChatInfo, policy: &Delegation) -> bool {
    read_skill_mark(home, chat).is_some_and(|mark| {
        orchestrator_told(Some(&mark.version), mark.delegation.as_deref(), policy)
    })
}

fn record_chat_skill(home: &Path, chat: &ChatInfo, policy: &Delegation) -> Result<(), String> {
    let path = skill_mark_path(home, chat.project_id.as_deref());
    let mark = SkillMark {
        chat_id: chat.id.clone(),
        version: orchestrator_skill_version(),
        delegation: Some(orchestrator_delegation_version(policy)),
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

/// The turn of one creator of `scope`'s orchestrator (`create`), held until the file it
/// returns is dropped: a lock file under `RIWORK_HOME/orchestrators`, so creators in
/// other processes (the desktop, the CLI, the MCP server) wait too.
fn lock_scope(home: &Path, scope: &OrchestratorScope) -> Result<fs::File, String> {
    let name = match scope {
        OrchestratorScope::Global => "global",
        OrchestratorScope::Project { project_id } => {
            if !Uuid::parse_str(project_id).is_ok_and(|id| id.to_string() == *project_id) {
                return Err(format!("invalid UUID: {project_id}"));
            }
            project_id.as_str()
        }
    };
    let directory = home.join("orchestrators");
    crate::paths::create_private_dir(&directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    let path = directory.join(format!("{name}.lock"));
    // A pure lock file: never truncate it, and keep other users from opening it.
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    file.lock()
        .map_err(|error| format!("lock {}: {error}", path.display()))?;
    Ok(file)
}

// ---- How they are shown -------------------------------------------------------------------

/// The agent behind a chat, as the CLI and the phone name it.
pub fn provider_word(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "codex",
        Provider::Claude => "claude",
        Provider::Muse => "muse",
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
/// `provider`), its `model` (null: the provider's default) and its state. `id` is the chat's id. `alive` is true for as long
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
        "model": chat.model,
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
