//! Handing a conversation over to another agent: `riwork handoff`, and the same from a tab's
//! menu.
//!
//! The source is a terminal shell or a chat. Its conversation goes into a Markdown document
//! in `RIWORK_HOME/handoffs/` (`document`), either as a transcript read from wherever that
//! kind of source keeps it (`sources`) or as a summary the live source agent was asked to
//! write (`summary`). The target is a new shell or chat, in the same project, worktree and
//! directory, with the model, effort, permissions and Codex account the request names
//! (`target`); its first message tells it to read the document. The source is left as it was.

use std::{
    fs,
    io::Write as _,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{
    chat::model::{ApprovalMode, ChatInfo, Provider},
    codex_accounts::CodexAccountBinding,
    sessions::{HarnessKind, SessionManager, ShellKind, ShellSession},
    store::Store,
};
use document::{BUDGET, Body, Header};
use summary::{Collected, Timing};

mod document;
mod sources;
pub mod summary;
mod target;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

/// A document shorter than this goes whole into a chat's first message; a longer one is
/// read from its file.
const INLINE_LIMIT: usize = 48 * 1024;
/// Shortest id prefix that names a shell or a chat.
const MIN_PREFIX: usize = 8;

/// What is handed over.
#[derive(Clone, Debug)]
pub enum Source {
    Shell(ShellSession),
    Chat(ChatInfo),
}

/// What the target is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Shell,
    Chat,
}

/// How the conversation is passed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    /// What the conversation says, written down from where it is kept.
    Transcript,
    /// What the agent says of it, when asked.
    Summary,
}

pub struct Request {
    pub source: Source,
    pub kind: Kind,
    pub provider: HarnessKind,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// A Codex account by label or id.
    pub account: Option<String>,
    pub mode: Option<ApprovalMode>,
    pub context: Context,
    pub note: Option<String>,
}

pub enum Started {
    Shell(ShellSession),
    Chat(ChatInfo),
}

pub struct Outcome {
    pub handoff_id: String,
    pub document: PathBuf,
    pub target: Started,
    /// What the document holds; a summary that could not be had is a transcript.
    pub context: Context,
    /// Why the summary was replaced by the transcript.
    pub fallback: Option<String>,
}

/// What a handoff needs from outside.
pub struct Env<'a> {
    pub home: &'a Path,
    /// Makes sure the chat host runs and says where it listens.
    pub ensure: &'a dyn Fn() -> Result<PathBuf, String>,
    /// The Codex account a person named (`codex_accounts::resolve_account`).
    pub account: &'a dyn Fn(&Path, &str) -> Result<CodexAccountBinding, String>,
    pub timing: Timing,
}

/// Where the source works, and so where the target does.
pub struct Origin {
    pub project_id: Option<String>,
    pub worktree_id: Option<String>,
    pub cwd: PathBuf,
}

// ---- The source --------------------------------------------------------------------------

impl Source {
    pub fn id(&self) -> &str {
        match self {
            Self::Shell(shell) => &shell.id,
            Self::Chat(chat) => &chat.id,
        }
    }

    /// The source as the document and the first message name it.
    pub fn label(&self) -> String {
        let short = &self.id()[..MIN_PREFIX.min(self.id().len())];
        match self {
            Self::Shell(shell) => match shell.harness {
                Some(harness) => format!("{} terminal {short}", agent_name(harness)),
                None => format!("terminal shell {short}"),
            },
            Self::Chat(chat) => chat_label(chat.provider, &chat.title, &chat.id),
        }
    }

    /// A Codex or Claude that can be asked something.
    pub fn is_askable(&self) -> bool {
        match self {
            Self::Chat(_) => true,
            Self::Shell(shell) => matches!(
                shell.harness,
                Some(HarnessKind::Codex | HarnessKind::Claude)
            ),
        }
    }

    fn project_id(&self) -> Option<&str> {
        match self {
            Self::Shell(shell) => shell.project_id.as_deref(),
            Self::Chat(chat) => chat.project_id.as_deref(),
        }
    }

    fn origin(&self, manager: Option<&SessionManager>) -> Origin {
        match self {
            Self::Shell(shell) => {
                // A plain shell goes where it has gone; an agent works where it started.
                let cwd = manager
                    .filter(|_| shell.harness.is_none() && shell.alive)
                    .and_then(|manager| manager.current_directory(&shell.id).ok())
                    .filter(|cwd| cwd.is_dir())
                    .unwrap_or_else(|| shell.cwd.clone());
                Origin {
                    project_id: shell.project_id.clone(),
                    worktree_id: shell.worktree_id.clone(),
                    cwd,
                }
            }
            Self::Chat(chat) => Origin {
                project_id: chat.project_id.clone(),
                worktree_id: chat.worktree_id.clone(),
                cwd: chat.cwd.clone(),
            },
        }
    }
}

/// A chat as the document and the first message name it: `Codex chat "Fix it" (1234abcd)`.
pub fn chat_label(provider: Provider, title: &str, id: &str) -> String {
    format!(
        "{} chat \"{}\" ({})",
        provider_name(provider),
        document::clip(title, 60),
        &id[..MIN_PREFIX.min(id.len())]
    )
}

pub fn agent_name(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::Codex => "Codex",
        HarnessKind::Claude => "Claude",
        HarnessKind::Grok => "Grok",
    }
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
    }
}

/// Which id to hand off from: the one given, else the session this process runs in.
/// A process in a terminal has `RIWORK_SHELL_ID`, in a chat `RIWORK_CHAT_ID`; one with
/// both (a terminal started by an agent in a chat, say) cannot say whose it is.
pub fn source_selector(
    from: Option<&str>,
    shell_env: Option<&str>,
    chat_env: Option<&str>,
) -> Result<String, String> {
    fn set(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|value| !value.is_empty())
    }
    if let Some(from) = set(from) {
        return Ok(from.to_owned());
    }
    match (set(shell_env), set(chat_env)) {
        (Some(shell), None) => Ok(shell.to_owned()),
        (None, Some(chat)) => Ok(chat.to_owned()),
        (Some(_), Some(_)) => Err(
            "Both RIWORK_SHELL_ID and RIWORK_CHAT_ID are set, so this command cannot tell which conversation is yours. Pass --from."
                .into(),
        ),
        (None, None) => Err(
            "Pass --from SHELL_OR_CHAT_ID: this command was not started by a RiWork shell or chat, so it has no conversation of its own to hand off."
                .into(),
        ),
    }
}

/// The shell or chat `selector` names: its whole id, or the start of it (at least eight
/// characters).
pub fn resolve_source(home: &Path, selector: &str) -> Result<Source, String> {
    let selector = selector.trim();
    let prefixed =
        |id: &str| id == selector || (selector.len() >= MIN_PREFIX && id.starts_with(selector));
    if selector.len() < MIN_PREFIX || !selector.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(format!(
            "'{selector}' is not a shell or chat id; ids need at least {MIN_PREFIX} characters when shortened"
        ));
    }
    let mut found: Vec<Source> = saved_chats(home)
        .into_iter()
        .filter(|chat| prefixed(&chat.id))
        .map(Source::Chat)
        .collect();
    // Without tmux there are no shells to find, and chats still can be.
    if let Ok(manager) = SessionManager::at(home.to_path_buf())
        && let Ok(shells) = manager.list()
    {
        found.extend(
            shells
                .into_iter()
                .filter(|shell| prefixed(&shell.id))
                .map(Source::Shell),
        );
    }
    // An exact id is never ambiguous.
    if let Some(exact) = found.iter().position(|source| source.id() == selector) {
        return Ok(found.swap_remove(exact));
    }
    match found.len() {
        0 => Err(format!("No shell or chat has the id {selector}")),
        1 => Ok(found.remove(0)),
        _ => Err(format!(
            "The id prefix {selector} matches more than one shell or chat"
        )),
    }
}

/// The chats on disk, from their `info.json`. The host need not run.
fn saved_chats(home: &Path) -> Vec<ChatInfo> {
    let Ok(entries) = fs::read_dir(crate::chat::log::chats_dir(home)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| fs::read_to_string(entry.path().join("info.json")).ok())
        .filter_map(|text| serde_json::from_str::<ChatInfo>(&text).ok())
        .collect()
}

// ---- Handing over ---------------------------------------------------------------------

/// Refuses what cannot be done before anything is read, asked or started.
pub fn check(request: &Request) -> Result<(), String> {
    if let Source::Shell(shell) = &request.source
        && shell.kind == ShellKind::Orchestrator
    {
        return Err("An orchestrator's conversation is not handed off.".into());
    }
    if request.kind == Kind::Chat {
        target::chat_provider(request.provider)?;
    }
    if request.kind == Kind::Shell && request.source.project_id().is_none() {
        return Err(
            "The source belongs to no project, and a terminal needs one to start in. Hand off to a chat instead."
                .into(),
        );
    }
    if request.account.is_some() && request.provider != HarnessKind::Codex {
        return Err(format!(
            "{} has no RiWork-managed accounts: it always uses the login of the system. Leave out --account; only Codex accounts can be chosen.",
            agent_name(request.provider)
        ));
    }
    if request.context == Context::Summary && !request.source.is_askable() {
        return Err(
            "A summary is written by the source agent when asked, and this source is not a Codex or Claude agent (a chat, or a terminal running one). Use --context transcript."
                .into(),
        );
    }
    Ok(())
}

/// Hands the conversation of `request.source` over to a new agent. `progress` hears what
/// the work is at ("Writing summary…") and may be called from any point of it; this blocks
/// for as long as the work takes, up to minutes when a summary is asked for, so call it
/// off the UI thread.
pub fn run(env: &Env<'_>, request: Request, progress: &dyn Fn(&str)) -> Result<Outcome, String> {
    run_with_creation(env, request, progress, true)
}

/// CLI handoffs inherit the caller's parent and worker provenance, like shell create.
pub fn run_from_caller(
    env: &Env<'_>,
    request: Request,
    progress: &dyn Fn(&str),
) -> Result<Outcome, String> {
    run_with_creation(env, request, progress, false)
}

fn run_with_creation(
    env: &Env<'_>,
    request: Request,
    progress: &dyn Fn(&str),
    user_action: bool,
) -> Result<Outcome, String> {
    check(&request)?;
    let home = std::path::absolute(env.home)
        .map_err(|error| format!("Cannot resolve {}: {error}", env.home.display()))?;
    // Shells need tmux; a chat to a chat does not.
    let needs_tmux = request.kind == Kind::Shell || matches!(request.source, Source::Shell(_));
    let manager = if needs_tmux {
        let manager = SessionManager::at(home.clone())?;
        Some(if user_action {
            manager.for_user()
        } else {
            manager
        })
    } else {
        None
    };
    let origin = request.source.origin(manager.as_ref());
    // The account is settled before any work is done for a target that cannot have it.
    let binding = match &request.account {
        Some(query) => Some((env.account)(&home, query)?),
        None => None,
    };
    let handoff_id = Uuid::new_v4().to_string();
    let dir = home.join("handoffs");
    crate::paths::create_private_dir(&dir)
        .map_err(|error| format!("Cannot create {}: {error}", dir.display()))?;

    let (read, context, fallback) = match request.context {
        Context::Transcript => {
            progress("Reading the conversation…");
            (
                read_source(&home, &request.source, manager.as_ref())?,
                Context::Transcript,
                None,
            )
        }
        Context::Summary => {
            progress("Writing summary…");
            let file = dir.join(format!("{handoff_id}.summary.md"));
            let collected = ask_for_summary(env, &home, &request.source, manager.as_ref(), &file)?;
            let _ = fs::remove_file(&file);
            match collected {
                Collected::Written(text) => (
                    sources::Read {
                        body: Body::Summary(text),
                        model: match &request.source {
                            Source::Chat(chat) => chat.model.clone(),
                            Source::Shell(_) => None,
                        },
                        origin: "the agent's own summary of it".into(),
                        caveat: None,
                    },
                    Context::Summary,
                    None,
                ),
                Collected::Missing(reason) => {
                    progress("No summary; reading the conversation…");
                    let mut read = read_source(&home, &request.source, manager.as_ref())?;
                    read.caveat = Some(format!(
                        "A summary was asked for, but {reason}; this is the transcript instead.{}",
                        read.caveat
                            .as_ref()
                            .map(|caveat| format!(" {caveat}"))
                            .unwrap_or_default()
                    ));
                    (read, Context::Transcript, Some(reason))
                }
            }
        }
    };

    let label = request.source.label();
    let (project, worktree) = names(&home, &origin);
    let header = Header {
        from: format!("{label}, from {}", read.origin),
        model: read.model.clone(),
        project,
        worktree,
        directory: origin.cwd.clone(),
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        note: request.note.as_deref().and_then(tidy_note),
        caveat: read.caveat.clone(),
    };
    let rendered = document::render(&header, &read.body, BUDGET);
    let document = write_document(&dir, &handoff_id, &rendered)?;

    let note = header.note.as_deref();
    let started = match request.kind {
        Kind::Chat => {
            progress("Starting the chat…");
            let message = if rendered.len() < INLINE_LIMIT {
                inline_message(&label, &rendered, note)
            } else {
                path_message(&label, &document, note)
            };
            let account = binding.as_ref().map(|binding| {
                binding
                    .id
                    .clone()
                    .unwrap_or_else(|| crate::codex_accounts::SYSTEM_DEFAULT_ID.to_owned())
            });
            let title = document::clip(&format!("Handoff from {label}"), 100);
            let socket = (env.ensure)()?;
            target::start_chat(&socket, &origin, &request, account, title, message)
                .map(Started::Chat)
                .map_err(|error| format!("{error} The handoff is in {}.", document.display()))?
        }
        Kind::Shell => {
            progress("Starting the terminal…");
            let manager = manager.as_ref().ok_or("tmux is required")?;
            let prompt = path_message(&label, &document, note);
            target::start_shell(manager, &origin, &request, binding, prompt)
                .map(Started::Shell)
                .map_err(|error| format!("{error} The handoff is in {}.", document.display()))?
        }
    };
    Ok(Outcome {
        handoff_id,
        document,
        target: started,
        context,
        fallback,
    })
}

fn read_source(
    home: &Path,
    source: &Source,
    manager: Option<&SessionManager>,
) -> Result<sources::Read, String> {
    match (source, manager) {
        (Source::Chat(chat), _) => sources::read_chat(home, &chat.id),
        (Source::Shell(shell), Some(manager)) => sources::read_shell(manager, shell),
        (Source::Shell(_), None) => Err("tmux is required to read a terminal".into()),
    }
}

fn ask_for_summary(
    env: &Env<'_>,
    home: &Path,
    source: &Source,
    manager: Option<&SessionManager>,
    file: &Path,
) -> Result<Collected, String> {
    match (source, manager) {
        (Source::Chat(chat), _) => {
            let socket = (env.ensure)()?;
            let mut agent = summary::ChatAgent::new(home.to_path_buf(), socket, chat.id.clone());
            summary::collect(&mut agent, file, env.timing)
        }
        (Source::Shell(shell), Some(manager)) => {
            let mut agent = summary::TerminalAgent::new(manager.clone(), shell.clone());
            summary::collect(&mut agent, file, env.timing)
        }
        (Source::Shell(_), None) => Err("tmux is required to ask a terminal".into()),
    }
}

/// The project's name and the worktree's branch, as the document names them.
fn names(home: &Path, origin: &Origin) -> (Option<String>, Option<String>) {
    let Ok(state) = Store::open(home).and_then(|store| store.snapshot()) else {
        return (None, None);
    };
    let project = origin
        .project_id
        .as_deref()
        .and_then(|id| state.project(id).ok())
        .map(|project| project.name.clone());
    let worktree = origin
        .worktree_id
        .as_deref()
        .and_then(|id| state.worktree(id).ok())
        .map(|worktree| format!("{} ({})", worktree.branch, worktree.path.display()));
    (project, worktree)
}

/// Writes the document owner-only, and never over another.
fn write_document(dir: &Path, id: &str, text: &str) -> Result<PathBuf, String> {
    let path = dir.join(format!("{id}.md"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| format!("Cannot write {}: {error}", path.display()))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("Cannot write {}: {error}", path.display()))?;
    Ok(path)
}

// ---- The first message -----------------------------------------------------------------

/// A note is one line, since it is typed into a terminal: its runs of spaces and its line
/// breaks become single spaces, and a blank one is none.
pub fn tidy_note(note: &str) -> Option<String> {
    let note = note.split_whitespace().collect::<Vec<_>>().join(" ");
    (!note.is_empty()).then_some(note)
}

fn with_note(mut message: String, note: Option<&str>) -> String {
    if let Some(note) = note {
        message.push_str(&format!(
            " Note from the person who handed this over: {note}"
        ));
    }
    message
}

/// The first message when the document is read from its file: for a terminal, whose
/// command line is short, and for a document too long to send whole.
fn path_message(from: &str, document: &Path, note: Option<&str>) -> String {
    with_note(
        format!(
            "You are taking over a conversation from {from}. Read the handoff at {} and continue from where it left off.",
            document.display()
        ),
        note,
    )
}

/// The first message with a short document in it.
fn inline_message(from: &str, document: &str, note: Option<&str>) -> String {
    format!(
        "{}\n\n<handoff>\n{}\n</handoff>",
        with_note(
            format!(
                "You are taking over a conversation from {from}. The handoff is below; continue from where it left off."
            ),
            note
        ),
        document.trim_end()
    )
}
