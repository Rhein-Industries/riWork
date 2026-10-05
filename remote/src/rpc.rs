//! Narrow CLI allowlist plus a durable write-ahead input outcome ledger.
use crate::{
    MAX_PLAINTEXT, appearance,
    config::{Storage, private_read, private_write, private_write_relaxed},
    crypto::uuid,
    pty::{self, PtyFault, PtySet},
    viewport::Viewport,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    time::{Duration, timeout},
};

mod chat;
mod orchestrator;
mod upload;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub method: String,
    pub params: Value,
}
#[derive(Debug)]
struct Fault {
    code: &'static str,
    message: String,
}
impl Fault {
    fn new(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            code,
            message: msg.into(),
        }
    }
}
fn invalid(e: impl std::fmt::Display) -> Fault {
    Fault::new("invalid_request", e.to_string())
}
impl From<PtyFault> for Fault {
    fn from(fault: PtyFault) -> Self {
        Self::new(fault.code, fault.message)
    }
}
/// The terminal streams of the connection a desktop request arrived on. A
/// device that may not have any (a phone) has none, and the methods do not exist
/// for it: exactly what a connector from before them would answer.
fn pty_set(pty: Option<&Arc<PtySet>>) -> std::result::Result<&Arc<PtySet>, Fault> {
    pty.ok_or_else(|| invalid("unsupported RPC method"))
}
fn cli_fault(e: impl std::fmt::Display) -> Fault {
    Fault::new("cli_error", e.to_string())
}
/// The CLI wrote more than one encrypted response can carry.
#[derive(Debug)]
struct OutputTooLarge;
impl std::fmt::Display for OutputTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CLI response exceeded limit")
    }
}
impl std::error::Error for OutputTooLarge {}
fn viewport_fault(e: impl std::fmt::Display) -> Fault {
    let message = e.to_string();
    let code = if message.contains("viewport_busy:") {
        "viewport_busy"
    } else if message.contains("viewport_unsupported:") {
        "viewport_unsupported"
    } else {
        "cli_error"
    };
    Fault::new(code, message)
}
pub fn error(id: &str, code: &str, message: impl AsRef<str>) -> Value {
    error_for(json!(id), code, message)
}
// `id` is null only when the request carried no usable ID to correlate with.
pub(crate) fn error_for(id: Value, code: &str, message: impl AsRef<str>) -> Value {
    json!({"v":1,"type":"response","id":id,"ok":false,"error":{"code":code,"message":message.as_ref()}})
}
fn success(id: &str, result: Value) -> Value {
    json!({"v":1,"type":"response","id":id,"ok":true,"result":result})
}
/// The request in `value` and its id, or the response to send instead.
/// Malformed requests get an error response, not a dropped session. Only an ID
/// that is a bounded string can be echoed for correlation.
fn parse_request(value: Value) -> std::result::Result<(String, Request), Value> {
    let request_id = match value.get("id") {
        Some(Value::String(s)) if s.len() <= 64 => s.clone(),
        _ => {
            return Err(error_for(
                Value::Null,
                "invalid_request",
                "request must be a JSON object with a string id of at most 64 bytes",
            ));
        }
    };
    match serde_json::from_value(value) {
        Ok(request) => Ok((request_id, request)),
        Err(e) => Err(error(&request_id, "invalid_request", e.to_string())),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Project {
    project_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tasks {
    project_id: String,
    worktree_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    shell_id: String,
    lines: Option<u32>,
    styled: Option<bool>,
    if_changed: Option<String>,
    wait_ms: Option<i64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    shell_id: String,
    end: u32,
    lines: u32,
    styled: Option<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    shell_id: String,
    line: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Keys {
    shell_id: String,
    batch: String,
    items: Vec<Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resize {
    shell_id: String,
    columns: u32,
    rows: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Clear {
    shell_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Close {
    shell_id: String,
}
fn params<T: serde::de::DeserializeOwned>(r: &Request) -> std::result::Result<T, Fault> {
    serde_json::from_value(r.params.clone()).map_err(invalid)
}
fn id(s: &str) -> std::result::Result<(), Fault> {
    uuid(s).map(|_| ()).map_err(invalid)
}
/// The longest a `shell.output` may wait for a change.
pub const MAX_WAIT_MS: i64 = 10_000;
/// The most lines one `shell.history` page may hold, as far as the connector goes. The installed
/// CLI limits itself (`src/sessions.rs`) and may be an older build; the first time it refuses a page
/// for being too long, `Rpc` remembers its limit (`Rpc::history_max_lines`) and says so in `ready`.
pub const HISTORY_PAGE_MAX: u32 = crate::link::HISTORY_MAX_LINES;
/// The limit the CLI names in `--lines needs an integer from 1 to N`, if that is what `message` says.
fn cli_lines_limit(message: &str) -> Option<u32> {
    let rest = message.split_once("--lines needs an integer from 1 to ")?.1;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|n| *n >= 1)
}
/// A CLI call that is not a wait. Above the tmux timeout of a single capture.
const CLI_TIMEOUT: Duration = Duration::from_secs(15);
/// What a waiting CLI call may take beyond its wait: the captures around it.
const CLI_WAIT_MARGIN: Duration = Duration::from_secs(8);
/// Starting a terminal. An agent's start-up (Grok waits for the computer-use
/// driver for up to 20 seconds first) is far above any other call, and the CLI
/// must not be cut off between creating the tmux session and writing it down.
const CREATE_TIMEOUT: Duration = Duration::from_secs(60);
/// The longest `command` a `shell.create` may carry.
pub const CREATE_COMMAND_MAX: usize = 4096;
/// The longest `name` a `project.create` may carry: this many characters (Unicode
/// scalar values), and, because the name becomes a folder name, this many UTF-8 bytes.
pub const PROJECT_NAME_MAX_CHARS: usize = 100;
pub const PROJECT_NAME_MAX_BYTES: usize = 255;

/// How long the CLI may take for a `shell.output` that waits up to `wait_ms`:
/// the wait plus the captures around it, and never less than any other call.
/// The CLI itself ends within about `wait_ms`, so this only trips when it hangs.
fn cli_limit(wait_ms: i64) -> Duration {
    CLI_TIMEOUT.max(Duration::from_millis(wait_ms.clamp(0, MAX_WAIT_MS) as u64) + CLI_WAIT_MARGIN)
}

/// A hash as `shell.output` hands them out: short, printable, no spaces.
fn hash_shaped(hash: &str) -> bool {
    (1..=64).contains(&hash.len()) && hash.bytes().all(|b| b.is_ascii_graphic())
}
/// Whether `text` holds no escape but well-formed SGR sequences
/// (`ESC [ digits ; : m`) and no control character but the newline and tab.
/// The CLI filters its styled output the same way; this refuses to pass on
/// what a CLI that did not.
fn sgr_only(text: &str) -> bool {
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                if chars.next() != Some('[') {
                    return false;
                }
                let mut length = 0;
                loop {
                    match chars.next() {
                        Some('m') => break,
                        Some(c) if c.is_ascii_digit() || c == ';' || c == ':' => length += 1,
                        _ => return false,
                    }
                    if length > 64 {
                        return false;
                    }
                }
            }
            '\n' | '\t' => {}
            c if c.is_control() => return false,
            _ => {}
        }
    }
    true
}
fn array(v: Value) -> std::result::Result<Vec<Value>, Fault> {
    v.as_array()
        .cloned()
        .ok_or_else(|| cli_fault("CLI did not return an array"))
}
fn project(v: Value, fields: &[&str]) -> Value {
    let mut m = serde_json::Map::new();
    for f in fields {
        if let Some(x) = v.get(*f).filter(|x| additive_shape_ok(f, x)) {
            m.insert((*f).into(), x.clone());
        }
    }
    Value::Object(m)
}
/// The additive fields (activity, recency) have a fixed shape for the phone. A
/// CLI that answers one in another shape has it left out, as if it had not
/// answered it, so a newer or damaged answer cannot reach a phone that decodes
/// these strictly. The older fields are passed on as they are.
fn additive_shape_ok(field: &str, value: &Value) -> bool {
    match field {
        "last_edited_unix" | "last_activity_unix" | "activity_since_unix" | "subagents_working" => {
            value.is_u64()
        }
        "activity" => value.as_str().is_some_and(|activity| {
            matches!(
                activity,
                "working" | "waiting" | "done" | "unknown" | "exited"
            )
        }),
        "agents" => value.as_object().is_some_and(|counts| {
            counts.len() <= 3
                && ["working", "waiting"]
                    .iter()
                    .all(|key| counts.get(*key).is_some_and(Value::is_u64))
                && counts.iter().all(|(key, count)| {
                    ["working", "waiting", "done"].contains(&key.as_str()) && count.is_u64()
                })
        }),
        // How a session runs. Only these two words are the phone's to act on.
        "mode" => matches!(value.as_str(), Some("terminal" | "chat")),
        "provider" => matches!(value.as_str(), Some("codex" | "claude")),
        "chat_id" => value.as_str().is_some_and(|chat| uuid(chat).is_ok()),
        "subagent_kinds" => value.as_array().is_some_and(|kinds| {
            kinds.len() <= 8
                && kinds.iter().all(|kind| {
                    kind.as_str().is_some_and(|kind| {
                        (1..=40).contains(&kind.len())
                            && kind
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"-_:.".contains(&b))
                    })
                })
        }),
        _ => true,
    }
}
/// The most a list or lookup may print. Lists carry fields the phone never
/// sees, such as each shell's launch command, so their raw output grows with
/// the number of shells while the reply stays small.
const MAX_LIST_OUTPUT: usize = 16 * 1024 * 1024;
/// What the phone may know of a project, in `projects.list` and in `project.create`.
/// `last_edited_unix` (when the desktop last saw a file of the project change) only
/// appears once the desktop app has published it. `last_activity_unix` (when one of
/// its shells last had output) and `agents` (the project's agents by state) are the
/// CLI's own, from tmux and the agents' files. See "Activity and recency extension" in
/// docs/remote-protocol.md.
const PROJECT_FIELDS: &[&str] = &[
    "id",
    "name",
    "root",
    "created_at",
    "last_edited_unix",
    "last_activity_unix",
    "agents",
];
const SESSION_FIELDS: &[&str] = &[
    "id",
    "project_id",
    "worktree_id",
    "kind",
    "cwd",
    "harness",
    "alive",
    "created_at_unix",
    "last_activity_unix",
    "activity",
    "activity_since_unix",
    "subagents_working",
    "subagent_kinds",
    "mode",
    "chat_id",
    "provider",
];
/// A session as the phone may see it: `SESSION_FIELDS`, each checked on its own.
/// `chat_id` and `provider` describe a chat, so they are passed on only for an
/// entry whose own `mode` is `chat` (judged after its shape check: a `mode` that
/// was left out as malformed makes the entry one without a chat). They are the
/// only fields whose meaning depends on another field of the same entry.
fn session_fields(v: Value) -> Value {
    let mut session = project(v, SESSION_FIELDS);
    if session.get("mode").and_then(Value::as_str) != Some("chat")
        && let Some(map) = session.as_object_mut()
    {
        map.remove("chat_id");
        map.remove("provider");
    }
    session
}
/// What a session the CLI listed with `mode` `chat` is: an orchestrator that runs
/// as a chat of the chat host. Its `id` is the chat's, not a tmux shell's.
fn runs_as_chat(session: &Value) -> bool {
    session.get("mode").and_then(Value::as_str) == Some("chat")
}
/// The answer to every `shell.*` method that names a chat orchestrator. It is
/// given before the CLI is asked to read or type into a shell that does not exist.
fn chat_orchestrator_fault() -> Fault {
    invalid(
        "this orchestrator runs as a chat; follow it with chat.events and send with chat.command",
    )
}
/// What `selected` and `Checked::explain` say of an id nobody knows.
const SHELL_NOT_FOUND: &str = "existing shell ID not found";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    digest: String,
    response: Option<Value>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    entries: BTreeMap<String, Record>,
}

/// What `shell.create` may start: a plain shell or one of the agent CLIs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CreateKind {
    Shell,
    Codex,
    Claude,
    Grok,
}
impl CreateKind {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "shell" => Self::Shell,
            "codex" => Self::Codex,
            "claude" => Self::Claude,
            "grok" => Self::Grok,
            _ => return None,
        })
    }
    /// The `--harness` value, `None` for a plain shell.
    fn harness(self) -> Option<&'static str> {
        match self {
            Self::Shell => None,
            Self::Codex => Some("codex"),
            Self::Claude => Some("claude"),
            Self::Grok => Some("grok"),
        }
    }
}
/// Exactly one of the two, never a name or a path: the CLI would resolve those.
#[derive(Debug, PartialEq, Eq)]
enum CreateTarget {
    Project(String),
    Worktree(String),
}
/// A validated `shell.create`.
#[derive(Debug, PartialEq, Eq)]
struct CreateSpec {
    target: CreateTarget,
    kind: CreateKind,
    unrestricted: bool,
    command: Option<String>,
}
const CREATE_FIELDS: [&str; 5] = [
    "project_id",
    "worktree_id",
    "kind",
    "unrestricted",
    "command",
];

/// The params of `shell.create`, strictly: an object with only the five fields
/// below, none of them null, before any CLI runs.
fn create_spec(params: &Value) -> std::result::Result<CreateSpec, Fault> {
    let object = params
        .as_object()
        .ok_or_else(|| invalid("params must be an object"))?;
    if let Some(unknown) = object.keys().find(|k| !CREATE_FIELDS.contains(&k.as_str())) {
        return Err(invalid(format!("unknown field {unknown}")));
    }
    let text = |name: &str| -> std::result::Result<Option<&str>, Fault> {
        match object.get(name) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(invalid(format!("{name} must be a string"))),
        }
    };
    let target = match (text("project_id")?, text("worktree_id")?) {
        (Some(project), None) => {
            id(project)?;
            CreateTarget::Project(project.to_owned())
        }
        (None, Some(worktree)) => {
            id(worktree)?;
            CreateTarget::Worktree(worktree.to_owned())
        }
        _ => return Err(invalid("give exactly one of project_id and worktree_id")),
    };
    let kind =
        text("kind")?.ok_or_else(|| invalid("kind is required: shell, codex, claude or grok"))?;
    let kind = CreateKind::parse(kind)
        .ok_or_else(|| invalid("kind must be shell, codex, claude or grok"))?;
    let unrestricted = match object.get("unrestricted") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(invalid("unrestricted must be a boolean")),
    };
    if unrestricted && kind.harness().is_none() {
        return Err(invalid(
            "unrestricted only applies to codex, claude and grok",
        ));
    }
    let command = match text("command")? {
        None => None,
        Some(command) => {
            if kind != CreateKind::Shell {
                return Err(invalid("command only applies to kind shell"));
            }
            create_command(command)?;
            Some(command.to_owned())
        }
    };
    Ok(CreateSpec {
        target,
        kind,
        unrestricted,
        command,
    })
}
/// A command for a plain shell: one physical line of at most 4096 bytes that
/// is not blank. It may not begin with `-`: the CLI reads such a value as the
/// next option, and no command starts that way.
fn create_command(command: &str) -> std::result::Result<(), Fault> {
    if command.trim().is_empty() || command.len() > CREATE_COMMAND_MAX {
        return Err(invalid(format!(
            "command must be 1..={CREATE_COMMAND_MAX} bytes and not blank"
        )));
    }
    if command
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        return Err(invalid(
            "command must be one line without control characters",
        ));
    }
    if command.starts_with('-') {
        return Err(invalid("command must not begin with -"));
    }
    Ok(())
}
/// The CLI's argv for a validated request. `--json` is added by `read`. Every
/// value is its own argument; nothing here passes through a shell.
fn create_args(spec: &CreateSpec) -> Vec<String> {
    let mut args: Vec<String> = vec!["shell".into(), "create".into()];
    match &spec.target {
        CreateTarget::Project(project) => args.extend(["--project".into(), project.clone()]),
        CreateTarget::Worktree(worktree) => args.extend(["--worktree".into(), worktree.clone()]),
    }
    if let Some(harness) = spec.kind.harness() {
        args.extend(["--harness".into(), harness.into()]);
        if spec.unrestricted {
            args.push("--unrestricted".into());
        }
    }
    if let Some(command) = &spec.command {
        args.extend(["--command".into(), command.clone()]);
    }
    args
}
/// What a failed `riwork shell create` says, by the first line of its error
/// (`riwork: ` already stripped). The wording belongs to the CLI and was
/// there before this method; the codes are the connector's.
fn create_fault(fault: Fault) -> Fault {
    if fault.code != "cli_error" {
        return fault;
    }
    if fault.message == "RiWork CLI timeout" {
        return Fault::new(
            "cli_error",
            "starting the terminal took too long and was stopped; check the terminal list before trying again",
        );
    }
    let Some(detail) = fault.message.strip_prefix("RiWork CLI failed: riwork: ") else {
        return fault;
    };
    let line = detail.lines().next().unwrap_or_default();
    let installed = |program: &str| line == format!("{program} is not installed or is not on PATH");
    if line.starts_with("No project matches '") {
        Fault::new("not_found", "project not found on the desktop")
    } else if line.starts_with("No worktree matches '") {
        Fault::new("not_found", "worktree not found on the desktop")
    } else if ["codex", "claude", "grok"].into_iter().any(installed)
        || line.starts_with("Cua Driver is not installed")
    {
        Fault::new("harness_unavailable", line)
    } else if line.starts_with("resolve ")
        && !line.starts_with("resolve RiWork executable")
        && line.ends_with("No such file or directory (os error 2)")
    {
        Fault::new(
            "not_found",
            "the folder of this project or worktree no longer exists on the desktop",
        )
    } else {
        Fault::new("cli_error", line)
    }
}
/// The `shell.create` result for the session the CLI printed, or `None` if it
/// is not the session that was asked for: a project shell with a canonical id,
/// in the requested project or worktree, running the requested agent.
fn create_result(spec: &CreateSpec, cli: &Value) -> Option<Value> {
    let shell = cli.get("id")?.as_str()?;
    id(shell).ok()?;
    let text = |name: &str| cli.get(name).and_then(Value::as_str);
    let in_target = match &spec.target {
        CreateTarget::Project(project) => text("project_id") == Some(project),
        CreateTarget::Worktree(worktree) => text("worktree_id") == Some(worktree),
    };
    if text("kind") != Some("project")
        || !in_target
        || text("harness") != spec.kind.harness()
        || text("cwd").is_none()
        || cli.get("alive")?.as_bool().is_none()
        || cli.get("created_at_unix")?.as_u64().is_none()
    {
        return None;
    }
    Some(json!({"shell_id": shell, "shell": session_fields(cli.clone())}))
}
/// What a failed `riwork shell close` says.
fn close_fault(fault: Fault) -> Fault {
    let unknown = fault
        .message
        .strip_prefix("RiWork CLI failed: riwork: ")
        .is_some_and(|detail| detail.starts_with("unknown shell "));
    if unknown {
        Fault::new("not_found", SHELL_NOT_FOUND)
    } else {
        fault
    }
}

/// A validated `project.create`: a name for the folder and the project, and whether the
/// new folder becomes a Git repository.
#[derive(Debug, PartialEq, Eq)]
struct ProjectSpec {
    name: String,
    git: bool,
}
const PROJECT_FIELDS_ACCEPTED: [&str; 2] = ["name", "git"];

/// The params of `project.create`, strictly: an object with `name` (a string) and
/// optionally `git` (a boolean, neither null), nothing else, before any CLI runs. There
/// is no path: the project is always made in the desktop's default projects folder.
fn project_spec(params: &Value) -> std::result::Result<ProjectSpec, Fault> {
    let object = params
        .as_object()
        .ok_or_else(|| invalid("params must be an object"))?;
    if let Some(unknown) = object
        .keys()
        .find(|k| !PROJECT_FIELDS_ACCEPTED.contains(&k.as_str()))
    {
        return Err(invalid(format!("unknown field {unknown}")));
    }
    let name = match object.get("name") {
        Some(Value::String(name)) => name,
        Some(_) => return Err(invalid("name must be a string")),
        None => return Err(invalid("name is required")),
    };
    project_name(name)?;
    let git = match object.get("git") {
        None => true,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(invalid("git must be a boolean")),
    };
    Ok(ProjectSpec {
        name: name.clone(),
        git,
    })
}
/// A project name is one visible folder name, as `paths::default_new_project_path`
/// requires of it, and is kept to what is safe to hand the CLI and to show:
///
/// - not empty, no whitespace at either end (the CLI trims, so a name that
///   differs from its trimmed form would not be the folder that was validated);
/// - at most 100 characters and 255 bytes (a folder name on the Mac);
/// - no `char::is_control` character and no U+2028 / U+2029;
/// - no `/` or `\`, and nothing that starts with `.` (which covers `.` and
///   `..`, and hidden names the desktop reserves for tools);
/// - nothing that starts with `-`, which the CLI would read as an option.
fn project_name(name: &str) -> std::result::Result<(), Fault> {
    if name.is_empty() {
        return Err(invalid("name must not be empty"));
    }
    if name.trim() != name {
        return Err(invalid("name must not start or end with whitespace"));
    }
    if name.chars().count() > PROJECT_NAME_MAX_CHARS || name.len() > PROJECT_NAME_MAX_BYTES {
        return Err(invalid(format!(
            "name must be at most {PROJECT_NAME_MAX_CHARS} characters and {PROJECT_NAME_MAX_BYTES} bytes"
        )));
    }
    if name
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        return Err(invalid("name must not contain control characters"));
    }
    if name.contains(['/', '\\']) {
        return Err(invalid("name must be one folder name: no / or \\"));
    }
    if name.starts_with('.') {
        return Err(invalid("name must not start with a dot"));
    }
    if name.starts_with('-') {
        return Err(invalid("name must not start with -"));
    }
    Ok(())
}
/// The CLI's argv for a validated request. `--json` is added by `read`. The name is
/// one argument after `--name`, never part of a string a shell reads. `--exclusive`
/// makes the CLI refuse a folder or project that is there already instead of
/// registering it; it is only sent to a CLI that said it knows the flag.
fn project_args(spec: &ProjectSpec) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "project".into(),
        "create".into(),
        "--name".into(),
        spec.name.clone(),
    ];
    if !spec.git {
        args.push("--no-git".into());
    }
    args.push("--exclusive".into());
    args
}
/// What a failed `riwork project create --exclusive` says, by the first line of its error
/// (`riwork: ` already stripped). The CLI marks a name, folder or project that is taken with
/// the token `already_exists: project ` or `already_exists: folder `; the sentences for the
/// phone are the connector's own and say nothing about where on the desktop it looked.
fn project_create_fault(fault: Fault, name: &str) -> Fault {
    if fault.code != "cli_error" {
        return fault;
    }
    if fault.message == "RiWork CLI timeout" {
        return Fault::new(
            "cli_error",
            "creating the project took too long and was stopped; check the project list before trying again",
        );
    }
    let Some(detail) = fault.message.strip_prefix("RiWork CLI failed: riwork: ") else {
        return fault;
    };
    let line = detail.lines().next().unwrap_or_default();
    if line.starts_with("already_exists: project ") {
        Fault::new(
            "already_exists",
            format!("A project named \"{name}\" already exists on the desktop"),
        )
    } else if line.starts_with("already_exists: folder ") {
        Fault::new(
            "already_exists",
            format!("A folder named \"{name}\" already exists in the desktop's projects folder"),
        )
    } else {
        Fault::new("cli_error", line)
    }
}
/// The `project.create` result for the project the CLI printed, or `None` if it is not the
/// project that was asked for: a canonical id, the name that was sent, and a root that is
/// an absolute path whose folder has that name (the default location is always
/// `DEFAULT_FOLDER/NAME`).
fn project_create_result(spec: &ProjectSpec, cli: &Value) -> Option<Value> {
    let text = |name: &str| cli.get(name).and_then(Value::as_str);
    let project_id = text("id")?;
    id(project_id).ok()?;
    let root = std::path::Path::new(text("root")?);
    if text("name") != Some(spec.name.as_str())
        || !root.is_absolute()
        || root.file_name().and_then(|n| n.to_str()) != Some(spec.name.as_str())
        || cli.get("created_at")?.as_u64().is_none()
    {
        return None;
    }
    Some(json!({"project_id": project_id, "project": project(cli.clone(), PROJECT_FIELDS)}))
}

/// Direct-typing limits. `src/session_keys.rs` enforces the same ones in the
/// CLI; the two test matrices must stay identical.
const KEYS_MAX_ITEMS: usize = 64;
const KEYS_MAX_TEXT_BYTES: usize = 4096;
const KEY_NAMES: [&str; 14] = [
    "Enter",
    "Tab",
    "BTab",
    "Escape",
    "Backspace",
    "Delete",
    "Up",
    "Down",
    "Left",
    "Right",
    "Home",
    "End",
    "PageUp",
    "PageDown",
];
/// Batches remembered per device, oldest pruned first.
pub const KEYS_LEDGER_MAX: usize = 4096;

fn valid_key(name: &str) -> bool {
    KEY_NAMES.contains(&name) || matches!(name.as_bytes(), [b'C', b'-', b'a'..=b'z'])
}
/// One `shell.keys` item as the CLI's argv encoding: `t:TEXT` or `k:KEY`.
fn key_item(index: usize, item: &Value) -> std::result::Result<(String, usize), Fault> {
    let n = index + 1;
    let object = item
        .as_object()
        .ok_or_else(|| invalid(format!("item {n} must be an object")))?;
    if object.keys().any(|k| k != "text" && k != "key") {
        return Err(invalid(format!("item {n} takes only \"text\" or \"key\"")));
    }
    match (object.get("text"), object.get("key")) {
        (Some(Value::String(text)), None) => {
            if text.is_empty() || text.len() > KEYS_MAX_TEXT_BYTES {
                return Err(invalid(format!(
                    "item {n}: text must be 1..={KEYS_MAX_TEXT_BYTES} bytes"
                )));
            }
            if text
                .chars()
                .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
            {
                return Err(invalid(format!(
                    "item {n}: text must not contain control characters; send a key for Enter or Tab"
                )));
            }
            Ok((format!("t:{text}"), text.len()))
        }
        (None, Some(Value::String(key))) => {
            if !valid_key(key) {
                return Err(invalid(format!("item {n}: unknown key")));
            }
            Ok((format!("k:{key}"), 0))
        }
        (Some(_), Some(_)) => Err(invalid(format!(
            "item {n} takes either \"text\" or \"key\", not both"
        ))),
        (None, None) => Err(invalid(format!("item {n} needs a \"text\" or a \"key\""))),
        _ => Err(invalid(format!("item {n}: text and key must be strings"))),
    }
}
/// The whole batch as CLI items: 1..=64 items, at most 4096 text bytes.
fn key_items(items: &[Value]) -> std::result::Result<Vec<String>, Fault> {
    if items.is_empty() || items.len() > KEYS_MAX_ITEMS {
        return Err(invalid(format!(
            "items must hold 1..={KEYS_MAX_ITEMS} entries"
        )));
    }
    let mut total = 0usize;
    let mut argv = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let (encoded, text_bytes) = key_item(index, item)?;
        total += text_bytes;
        argv.push(encoded);
    }
    if total > KEYS_MAX_TEXT_BYTES {
        return Err(invalid(format!(
            "items may hold at most {KEYS_MAX_TEXT_BYTES} text bytes in all"
        )));
    }
    Ok(argv)
}
/// What a failed `riwork shell keys` says about the batch. A CLI error that
/// begins with one of its tokens happened before any key was typed, so the
/// batch can be forgotten and retried; anything else may have typed part of it.
/// Only the first line of stderr counts: text a shell echoes cannot forge it.
fn keys_fault(error: &anyhow::Error) -> (Fault, bool) {
    let message = error.to_string();
    let detail = message.strip_prefix("RiWork CLI failed: riwork: ");
    let tokens: [(&str, &'static str); 4] = [
        ("input_unavailable: ", "input_unavailable"),
        ("not_found: ", "not_found"),
        ("invalid_request: ", "invalid_request"),
        ("not_sent: ", "cli_error"),
    ];
    if let Some(detail) = detail {
        // A CLI from before direct typing has no `shell keys`: nothing was typed.
        if detail.starts_with("Unknown shell command 'keys'") {
            return (
                Fault::new(
                    "cli_error",
                    "the installed riwork CLI does not support shell keys; update RiWork",
                ),
                true,
            );
        }
        for (token, code) in tokens {
            if let Some(rest) = detail.strip_prefix(token) {
                return (
                    Fault::new(code, rest.lines().next().unwrap_or_default()),
                    true,
                );
            }
        }
    }
    (
        Fault::new(
            "cli_error",
            format!(
                "{message}; the batch may be partly typed, so repeating its batch UUID reports uncertain"
            ),
        ),
        false,
    )
}
/// What a failed `riwork shell history --json` says.
fn history_fault(fault: Fault) -> Fault {
    if fault.code == "response_too_large" {
        return Fault::new(
            "response_too_large",
            "the page exceeds the encrypted response limit; request fewer lines",
        );
    }
    // A CLI from before scrollback paging has no `shell history`.
    if fault.code == "cli_error" && fault.message.contains("Unknown shell command 'history'") {
        return Fault::new(
            "cli_error",
            "the installed riwork CLI does not support shell history; update RiWork",
        );
    }
    fault
}
fn appearance_not_published() -> Fault {
    Fault::new("not_found", "appearance not published")
}
/// What a failed `riwork appearance --json` says. The CLI reports a missing or
/// invalid file with one message; output past the document limit is invalid too.
fn appearance_fault(error: &anyhow::Error) -> Fault {
    if error.is::<OutputTooLarge>() {
        return appearance_not_published();
    }
    let message = error.to_string();
    if let Some(detail) = message.strip_prefix("RiWork CLI failed: riwork: ") {
        if detail.starts_with("RiWork has not published its appearance yet") {
            return appearance_not_published();
        }
        // A CLI from before theme sync has no `appearance` command.
        if detail.starts_with("'appearance' is not a riwork command") {
            return Fault::new(
                "cli_error",
                "the installed riwork CLI does not support appearance; update RiWork",
            );
        }
    }
    cli_fault(error)
}
/// The screen fields of the CLI's `shell output`, all or none: a cursor that
/// does not land on the last `rows` lines of the text is not passed on.
fn screen_fields(cli: &Value, output: &str) -> Option<Value> {
    let number = |v: &Value| v.as_u64().and_then(|n| u32::try_from(n).ok());
    let cursor = cli.get("cursor")?;
    let (x, y) = (number(cursor.get("x")?)?, number(cursor.get("y")?)?);
    let (rows, cols) = (number(cli.get("rows")?)?, number(cli.get("cols")?)?);
    let in_mode = cli.get("in_mode")?.as_bool()?;
    if rows == 0 || cols == 0 || y >= rows || output.lines().count() < rows as usize {
        return None;
    }
    Some(json!({"cursor":{"x":x,"y":y},"rows":rows,"cols":cols,"in_mode":in_mode}))
}

/// `history_size` and `alternate` of the CLI's `shell output`, both or neither:
/// the scrollback lines above the screen, and whether a full-screen program is
/// on the alternate screen.
fn scrollback_fields(cli: &Value) -> Option<(u32, bool)> {
    let size = cli.get("history_size")?.as_u64()?;
    Some((u32::try_from(size).ok()?, cli.get("alternate")?.as_bool()?))
}
/// What `shell.history` answers for the CLI's `riwork shell history --json`
/// page, or `None` if it is not a page of at most `lines` lines: `output` is
/// the lines joined by newlines, `line_count` of them (none is `""` with a
/// count of 0, one blank line is `""` with a count of 1).
fn history_result(shell: &str, cli: &Value, lines: u32, styled: bool) -> Option<Value> {
    let text = cli.get("output")?.as_str()?;
    let number = |name: &str| cli.get(name)?.as_u64().and_then(|n| u32::try_from(n).ok());
    let (line_count, history_size) = (number("line_count")?, number("history_size")?);
    let complete = cli.get("complete")?.as_bool()?;
    let found = if line_count == 0 {
        usize::from(!text.is_empty())
    } else {
        text.split('\n').count()
    };
    // A page is never empty unless it is the end of the history, and never
    // holds more than was asked for.
    let coherent = found == line_count as usize
        && line_count <= lines
        && (line_count > 0 || complete)
        && (line_count == 0 || line_count <= history_size);
    if !coherent || (styled && !sgr_only(text)) {
        return None;
    }
    Some(json!({
        "shell_id": shell,
        "output": text,
        "line_count": line_count,
        "history_size": history_size,
        "complete": complete
    }))
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BatchState {
    /// Recorded before typing. If it is still pending, the outcome is unknown.
    Pending,
    Sent,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchRecord {
    batch: String,
    state: BatchState,
}
/// Direct-typing batches of one device, oldest first, in `keys-DEVICE.json`.
/// Deliberately not the input outcome ledger: batches are keyed by their own
/// UUID, carry no request digest, and are pruned instead of filling up.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchLedger {
    batches: Vec<BatchRecord>,
}

/// How a request reaches the viewport of its connection.
enum ViewportAccess<'a> {
    /// No connection behind the request: resizing is refused.
    Absent,
    /// The caller has the viewport to itself for this one call.
    Owned(&'a mut Viewport),
    /// Concurrent requests share it. `None` once the connection is over.
    Shared(&'a tokio::sync::Mutex<Option<Viewport>>),
}
impl<'a> ViewportAccess<'a> {
    async fn hold(self) -> HeldViewport<'a> {
        match self {
            Self::Absent => HeldViewport::Absent,
            Self::Owned(viewport) => HeldViewport::Owned(viewport),
            Self::Shared(lock) => HeldViewport::Shared(lock.lock().await),
        }
    }
}
enum HeldViewport<'a> {
    Absent,
    Owned(&'a mut Viewport),
    Shared(tokio::sync::MutexGuard<'a, Option<Viewport>>),
}
impl HeldViewport<'_> {
    fn get(&mut self) -> Option<&mut Viewport> {
        match self {
            Self::Absent => None,
            Self::Owned(viewport) => Some(viewport),
            Self::Shared(guard) => guard.as_mut(),
        }
    }
}

/// Who made sure a shell is an existing, live session.
#[derive(Clone, Copy)]
enum Checked {
    /// This connector, by listing sessions.
    Here,
    /// The CLI, as part of the call it was then asked to make.
    ByCli,
}
impl Checked {
    /// A refusal of the CLI that says the shell is unknown or has exited, as
    /// the lookup would have said it. Any other failure is returned as it is,
    /// and so is every failure of a lookup made here. The CLI's wording is
    /// matched whole, so nothing else can pass for it.
    fn explain(self, fault: Fault, shell: &str) -> Fault {
        if matches!(self, Self::Here) || !matches!(fault.code, "cli_error" | "not_found") {
            return fault;
        }
        // From `shell output` and `shell history` a failure arrives as the
        // CLI's stderr, from `shell keys` as what follows its `not_found: `.
        let said = fault
            .message
            .strip_prefix("RiWork CLI failed: riwork: ")
            .unwrap_or(&fault.message);
        if said == format!("unknown shell {shell}") {
            Fault::new("not_found", SHELL_NOT_FOUND)
        } else if said == format!("shell {shell} has exited") {
            Fault::new("not_found", "selected shell is not alive")
        } else {
            fault
        }
    }
}

pub struct Rpc {
    pub cli: PathBuf,
    pub storage: Storage,
    input_locks: Mutex<BTreeMap<String, Weak<tokio::sync::Mutex<()>>>>,
    /// The longest `shell.history` page accepted: `HISTORY_PAGE_MAX`, or what the installed CLI has been seen to take.
    history_cap: AtomicU32,
    /// Whether the CLI itself refuses a shell that is not registered and alive
    /// (see `shell_checked_by_cli`), learned once from the CLI.
    cli_checks_shells: tokio::sync::OnceCell<bool>,
    /// Whether the CLI said it has `shell attach --exec` (see `require_attach_exec`).
    attach_exec: AtomicBool,
    /// Whether the CLI said it has the chat commands (see `chat_supported`).
    chat: AtomicBool,
    /// Whether the CLI said it can create orchestrators (see `orchestrator_create_supported`).
    orchestrator_create: AtomicBool,
    /// Held while the CLI is asked what it can do (`capability_known`), so that two askers at
    /// once run it once.
    asking_chat: tokio::sync::Mutex<()>,
    /// Whether the CLI said it has `shell paste` (see `require_shell_paste`).
    shell_paste: AtomicBool,
    /// Files the phones sent (see `crate::upload`).
    uploads: Arc<crate::upload::Uploads>,
}
impl Rpc {
    pub fn new(cli: PathBuf, storage: Storage) -> Self {
        Self {
            cli,
            input_locks: Mutex::new(BTreeMap::new()),
            history_cap: AtomicU32::new(HISTORY_PAGE_MAX),
            cli_checks_shells: tokio::sync::OnceCell::new(),
            attach_exec: AtomicBool::new(false),
            chat: AtomicBool::new(false),
            orchestrator_create: AtomicBool::new(false),
            asking_chat: tokio::sync::Mutex::new(()),
            shell_paste: AtomicBool::new(false),
            uploads: Arc::new(crate::upload::Uploads::new(storage.dir.clone())),
            storage,
        }
    }
    /// The most lines a `shell.history` page may have right now (announced in `ready`).
    pub fn history_max_lines(&self) -> u32 {
        self.history_cap.load(Ordering::Relaxed)
    }
    fn input_lock(&self, shell: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.input_locks.lock().expect("input lock registry");
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(shell).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(shell.into(), Arc::downgrade(&lock));
        lock
    }
    async fn raw(&self, args: Vec<String>) -> Result<Vec<u8>> {
        self.raw_within(args, CLI_TIMEOUT).await
    }
    async fn raw_within(&self, args: Vec<String>, limit: Duration) -> Result<Vec<u8>> {
        self.raw_capped(args, limit, MAX_PLAINTEXT).await
    }
    /// `cap`: the most the CLI may write to stdout before it is cut off. Replies
    /// that may be compressed (`handle_shared_up_to`) let the CLI write more than
    /// one encrypted frame holds; the reply itself is checked after.
    async fn raw_capped(&self, args: Vec<String>, limit: Duration, cap: usize) -> Result<Vec<u8>> {
        let mut child = Command::new(&self.cli)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("start configured RiWork CLI {}", self.cli.display()))?;
        let stdout = child.stdout.take().context("stdout")?;
        let stderr = child.stderr.take().context("stderr")?;
        let run = async {
            let out = async {
                let mut b = vec![];
                stdout.take((cap + 1) as u64).read_to_end(&mut b).await?;
                ensure!(b.len() <= cap, OutputTooLarge);
                Ok::<_, anyhow::Error>(b)
            };
            let err = async {
                let mut b = vec![];
                stderr.take(4097).read_to_end(&mut b).await?;
                ensure!(b.len() <= 4096, "CLI diagnostics exceeded limit");
                Ok::<_, anyhow::Error>(b)
            };
            let (out, err, status) = tokio::try_join!(out, err, async {
                Ok::<_, anyhow::Error>(child.wait().await?)
            })?;
            ensure!(
                status.success(),
                "RiWork CLI failed: {}",
                String::from_utf8_lossy(&err).trim()
            );
            Ok(out)
        };
        timeout(limit, run).await.context("RiWork CLI timeout")?
    }
    /// A list or lookup (projects, worktrees, shells, tasks, `show`). Its raw
    /// output is read under `MAX_LIST_OUTPUT`, not the reply limit: what the
    /// phone gets is projected to a few fields first (a shell's launch command
    /// alone can be kilobytes), and the reply is held to the encrypted response
    /// limit when it is sent.
    async fn read(&self, args: &[&str]) -> std::result::Result<Value, Fault> {
        self.read_capped(
            args.iter().map(|x| (*x).to_owned()).collect(),
            CLI_TIMEOUT,
            MAX_LIST_OUTPUT,
        )
        .await
    }
    async fn read_within(
        &self,
        a: Vec<String>,
        limit: Duration,
    ) -> std::result::Result<Value, Fault> {
        self.read_capped(a, limit, MAX_PLAINTEXT).await
    }
    async fn read_capped(
        &self,
        mut a: Vec<String>,
        limit: Duration,
        cap: usize,
    ) -> std::result::Result<Value, Fault> {
        a.push("--json".into());
        let data = self.raw_capped(a, limit, cap).await.map_err(|e| {
            if e.is::<OutputTooLarge>() {
                Fault::new(
                    "response_too_large",
                    "CLI output exceeds the encrypted response limit; reduce output lines",
                )
            } else {
                cli_fault(e)
            }
        })?;
        serde_json::from_slice(&data).map_err(cli_fault)
    }
    /// The session `shell` among the project shells, and if it is not one of
    /// them, among the orchestrators. Ids are unique across both, so asking the
    /// second list only when the first does not hold it finds exactly what
    /// asking both would, with one CLI process fewer for every project shell.
    async fn session(&self, shell: &str) -> std::result::Result<Option<Value>, Fault> {
        for list in [
            &["shell", "list", "--all"][..],
            &["orchestrator", "list"][..],
        ] {
            let found = array(self.read(list).await?)?
                .into_iter()
                .find(|s| s.get("id").and_then(Value::as_str) == Some(shell));
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }
    async fn selected(&self, shell: &str) -> std::result::Result<(), Fault> {
        id(shell)?;
        let Some(s) = self.session(shell).await? else {
            return Err(Fault::new("not_found", SHELL_NOT_FOUND));
        };
        // Before `alive` and before any `riwork shell ...`: a chat has no pane.
        if runs_as_chat(&s) {
            return Err(chat_orchestrator_fault());
        }
        if s.get("alive").and_then(Value::as_bool) != Some(true) {
            return Err(Fault::new("not_found", "selected shell is not alive"));
        }
        Ok(())
    }
    /// `checked.explain`, and, for a CLI that checked the shell itself and said it
    /// is unknown, one more look at the sessions: a chat orchestrator is not in
    /// the CLI's shell registry, so that is how it refuses one, and the phone
    /// should be told what it is rather than that it does not exist. The lookup
    /// is made only after such a refusal, so a call that works costs no more than
    /// before, and one that fails any other way is returned as it is.
    async fn explain(&self, checked: Checked, fault: Fault, shell: &str) -> Fault {
        let fault = checked.explain(fault, shell);
        if matches!(checked, Checked::ByCli)
            && fault.code == "not_found"
            && fault.message == SHELL_NOT_FOUND
            && let Ok(Some(found)) = self.session(shell).await
            && runs_as_chat(&found)
        {
            return chat_orchestrator_fault();
        }
        fault
    }
    /// Whether the installed CLI can become a tmux client in place
    /// (`shell attach ID --exec`, `riwork capabilities`). Only a yes is
    /// remembered: a CLI updated while the connector runs is believed at once,
    /// and an older one would refuse the flags as a usage error.
    async fn require_attach_exec(&self) -> std::result::Result<(), Fault> {
        if self.attach_exec.load(Ordering::Relaxed) {
            return Ok(());
        }
        let too_old = || {
            Fault::new(
                "cli_error",
                "the installed riwork CLI cannot open terminal streams; update RiWork",
            )
        };
        match self.raw(vec!["capabilities".into(), "--json".into()]).await {
            Ok(data) => {
                let reply = serde_json::from_slice::<Value>(&data).unwrap_or(Value::Null);
                if reply.get("v") == Some(&json!(1))
                    && reply.get("shell_attach_exec") == Some(&Value::Bool(true))
                {
                    self.attach_exec.store(true, Ordering::Relaxed);
                    Ok(())
                } else {
                    Err(too_old())
                }
            }
            // It ran and refused the question: a CLI from before capabilities.
            Err(e) if e.to_string().starts_with("RiWork CLI failed") => Err(too_old()),
            Err(e) => Err(cli_fault(e)),
        }
    }
    /// Whether this CLI checks, before it reads from or types into a shell,
    /// that the shell is a registered, live session, and says so in its own
    /// words (`riwork capabilities`). A CLI that predates the question, or a
    /// stand-in that does not answer it, is taken not to, and that is
    /// remembered like a yes. A CLI that could not be run at all is not
    /// remembered as anything: the next request asks again.
    async fn shell_checked_by_cli(&self) -> bool {
        self.cli_checks_shells
            .get_or_try_init(|| async {
                match self.raw(vec!["capabilities".into(), "--json".into()]).await {
                    Ok(data) => Ok(serde_json::from_slice::<Value>(&data).is_ok_and(|reply| {
                        reply.get("v") == Some(&json!(1))
                            && reply.get("verifies_shell") == Some(&Value::Bool(true))
                    })),
                    // It ran and refused the question.
                    Err(e) if e.to_string().starts_with("RiWork CLI failed") => Ok(false),
                    Err(_) => Err(()),
                }
            })
            .await
            .copied()
            .unwrap_or(false)
    }
    /// Makes sure `shell` is an existing, live session before the CLI is asked
    /// about it. Looking it up costs two CLI processes' worth of time; a CLI
    /// that checks for itself, in the very process that then reads or types
    /// (so just before, never earlier), makes that unnecessary, and
    /// `Checked::explain` puts its refusal in the words this lookup uses.
    async fn ensure_selected(&self, shell: &str) -> std::result::Result<Checked, Fault> {
        id(shell)?;
        if self.shell_checked_by_cli().await {
            return Ok(Checked::ByCli);
        }
        self.selected(shell).await?;
        Ok(Checked::Here)
    }
    pub async fn handle(&self, device: &str, value: Value) -> Result<Value> {
        self.handle_with(device, value, ViewportAccess::Absent)
            .await
    }
    /// One request on a connection that owns its viewport for the call.
    pub async fn handle_in(
        &self,
        device: &str,
        value: Value,
        viewport: Option<&mut Viewport>,
    ) -> Result<Value> {
        let viewport = viewport.map_or(ViewportAccess::Absent, ViewportAccess::Owned);
        self.handle_with(device, value, viewport).await
    }
    /// One request among concurrent ones on a connection: only the methods
    /// that resize take the viewport lock, and hold it while they run.
    pub async fn handle_shared(
        &self,
        device: &str,
        value: Value,
        viewport: &tokio::sync::Mutex<Option<Viewport>>,
    ) -> Result<Value> {
        self.handle_shared_up_to(device, value, viewport, MAX_PLAINTEXT)
            .await
    }
    /// Like `handle_shared`, for a connection whose replies may be compressed: a
    /// response may be up to `reply_limit` bytes of JSON (`link::MAX_INFLATED` at
    /// most) instead of one frame's worth. Whether it then fits a frame is up to
    /// the caller, which has the compressed size.
    pub async fn handle_shared_up_to(
        &self,
        device: &str,
        value: Value,
        viewport: &tokio::sync::Mutex<Option<Viewport>>,
        reply_limit: usize,
    ) -> Result<Value> {
        self.handle_session_up_to(device, value, viewport, None, reply_limit)
            .await
    }
    /// Like `handle_shared_up_to`, for a session that may also have terminal
    /// streams (`pty`): `pty` is its stream set, `None` for a device that may not
    /// open any, for which `pty.*` is an unsupported method.
    pub async fn handle_session_up_to(
        &self,
        device: &str,
        value: Value,
        viewport: &tokio::sync::Mutex<Option<Viewport>>,
        pty: Option<&Arc<PtySet>>,
        reply_limit: usize,
    ) -> Result<Value> {
        self.handle_limited(
            device,
            value,
            ViewportAccess::Shared(viewport),
            pty,
            reply_limit.clamp(MAX_PLAINTEXT, crate::link::MAX_INFLATED),
        )
        .await
    }
    /// `pty.write`, `pty.resize` or `pty.close`, which the connection loop
    /// answers itself, in arrival order (they never wait). The same checks as
    /// every other request; the device's authorization is looked at again
    /// because the loop does not start a task, and so does not reach the check
    /// that long requests get.
    pub fn handle_pty_inline(&self, device: &str, value: Value, set: &PtySet) -> Result<Value> {
        let (request_id, request) = match parse_request(value) {
            Ok(parsed) => parsed,
            Err(response) => return Ok(response),
        };
        let result = if request.v != 1 || request.kind != "request" {
            Err(invalid("unsupported request version/type"))
        } else if let Err(e) = id(&request.id) {
            Err(e)
        } else if !self.storage.authorized(device)? {
            return Err(anyhow::anyhow!("device revoked"));
        } else {
            pty::inline(set, &request.method, &request.params).map_err(Fault::from)
        };
        Ok(match result {
            Ok(v) => success(&request_id, v),
            Err(f) => error(&request_id, f.code, f.message),
        })
    }
    async fn handle_with(
        &self,
        device: &str,
        value: Value,
        viewport: ViewportAccess<'_>,
    ) -> Result<Value> {
        self.handle_limited(device, value, viewport, None, MAX_PLAINTEXT)
            .await
    }
    async fn handle_limited(
        &self,
        device: &str,
        value: Value,
        viewport: ViewportAccess<'_>,
        pty: Option<&Arc<PtySet>>,
        reply_limit: usize,
    ) -> Result<Value> {
        let (request_id, request) = match parse_request(value) {
            Ok(parsed) => parsed,
            Err(response) => return Ok(response),
        };
        let result = if request.v != 1 || request.kind != "request" {
            Err(invalid("unsupported request version/type"))
        } else if let Err(e) = id(&request.id) {
            Err(e)
        } else if !self.storage.authorized(device)? {
            return Err(anyhow::anyhow!("device revoked"));
        } else {
            let ledger_path = self.storage.dir.join(format!("outcomes-{device}.json"));
            // Terminal streams are not recorded, and `pty.read` comes many times a second.
            if request.method != "shell.input"
                && !request.method.starts_with("pty.")
                && ledger_path.exists()
            {
                let ledger: Ledger = private_read(&ledger_path, 64 * 1024 * 1024)?;
                if ledger.entries.contains_key(&request.id) {
                    return Ok(error(
                        &request.id,
                        "request_conflict",
                        "request UUID already used for input",
                    ));
                }
            }
            self.dispatch(device, &request, viewport, pty, reply_limit)
                .await
        };
        let response = match result {
            Ok(v) => success(&request_id, v),
            Err(f) => error(&request_id, f.code, f.message),
        };
        if serde_json::to_vec(&response)?.len() > reply_limit {
            return Ok(error(
                &request_id,
                "response_too_large",
                "result exceeds encrypted response limit; reduce output lines",
            ));
        }
        Ok(response)
    }
    async fn dispatch(
        &self,
        device: &str,
        r: &Request,
        viewport: ViewportAccess<'_>,
        pty: Option<&Arc<PtySet>>,
        reply_limit: usize,
    ) -> std::result::Result<Value, Fault> {
        match r.method.as_str() {
            "projects.list" => {
                let _: Empty = params(r)?;
                let v = array(self.read(&["project", "list"]).await?)?
                    .into_iter()
                    .map(|v| project(v, PROJECT_FIELDS))
                    .collect::<Vec<_>>();
                Ok(json!({"projects":v}))
            }
            "worktrees.list" => {
                let p: Project = params(r)?;
                id(&p.project_id)?;
                let v = array(
                    self.read(&["worktree", "list", "--project", &p.project_id])
                        .await?,
                )?
                .into_iter()
                .map(|v| {
                    project(
                        v,
                        &[
                            "id",
                            "project_id",
                            "branch",
                            "path",
                            "is_primary",
                            "created_at",
                        ],
                    )
                })
                .collect::<Vec<_>>();
                Ok(json!({"worktrees":v}))
            }
            "tasks.list" => {
                let p: Tasks = params(r)?;
                id(&p.project_id)?;
                let tasks = if let Some(w) = p.worktree_id {
                    id(&w)?;
                    let trees = array(
                        self.read(&["worktree", "list", "--project", &p.project_id])
                            .await?,
                    )?;
                    if !trees
                        .iter()
                        .any(|t| t.get("id").and_then(Value::as_str) == Some(&w))
                    {
                        return Err(Fault::new(
                            "not_found",
                            "worktree does not belong to selected project",
                        ));
                    }
                    self.read(&["task", "list", "--worktree", &w]).await?
                } else {
                    self.read(&["task", "list", "--project", &p.project_id])
                        .await?
                };
                Ok(
                    json!({"tasks":array(tasks)?.into_iter().map(|v|project(v,&["id","project_id","title","details","status","worktree_id","created_at","updated_at"])).collect::<Vec<_>>()}),
                )
            }
            "shells.list" => {
                let p: Project = params(r)?;
                id(&p.project_id)?;
                Ok(
                    json!({"shells":array(self.read(&["shell","list","--project",&p.project_id]).await?)?.into_iter().map(session_fields).collect::<Vec<_>>()}),
                )
            }
            "orchestrators.list" => {
                let _: Empty = params(r)?;
                Ok(
                    json!({"orchestrators":array(self.read(&["orchestrator","list"]).await?)?.into_iter().map(session_fields).collect::<Vec<_>>()}),
                )
            }
            "shell.output" => {
                let p: Output = params(r)?;
                let lines = p.lines.unwrap_or(200);
                if !(1..=2000).contains(&lines) {
                    return Err(invalid("lines must be 1..2000"));
                }
                let wait_ms = p.wait_ms.unwrap_or(0);
                if !(0..=MAX_WAIT_MS).contains(&wait_ms) {
                    return Err(invalid(format!("wait_ms must be 0..{MAX_WAIT_MS}")));
                }
                if p.if_changed.as_deref().is_some_and(|h| !hash_shaped(h)) {
                    return Err(invalid(
                        "if_changed must be the hash of an earlier result: 1..64 printable characters",
                    ));
                }
                let styled = p.styled.unwrap_or(false);
                let checked = self.ensure_selected(&p.shell_id).await?;
                let mut args: Vec<String> = [
                    "shell",
                    "output",
                    p.shell_id.as_str(),
                    "--lines",
                    &lines.to_string(),
                ]
                .into_iter()
                .map(String::from)
                .collect();
                if styled {
                    args.push("--styled".into());
                }
                let mut limit = CLI_TIMEOUT;
                if let Some(hash) = &p.if_changed {
                    // `=`: whatever the hash looks like, it stays one value.
                    args.push(format!("--if-changed={hash}"));
                    // A wait without a hash to compare has nothing to wait for.
                    args.push("--wait-ms".into());
                    args.push(wait_ms.to_string());
                    limit = cli_limit(wait_ms);
                }
                let v = match self.read_capped(args, limit, reply_limit).await {
                    Ok(v) => v,
                    Err(fault) => {
                        let fault = self.explain(checked, fault, &p.shell_id).await;
                        // A CLI from before styled output and waiting refuses the flags.
                        return Err(
                            if fault.code == "cli_error"
                                && fault.message.contains("Usage: riwork shell output")
                                && (styled || p.if_changed.is_some())
                            {
                                Fault::new(
                                    "cli_error",
                                    "the installed riwork CLI does not support styled output or waiting for changes; update RiWork",
                                )
                            } else {
                                fault
                            },
                        );
                    }
                };
                let hash = v
                    .get("hash")
                    .and_then(Value::as_str)
                    .filter(|h| hash_shaped(h));
                if v.get("unchanged") == Some(&Value::Bool(true)) {
                    // Only ever the answer to a hash this request sent.
                    return match (&p.if_changed, hash) {
                        (Some(asked), Some(hash)) if asked == hash => {
                            let mut result =
                                json!({"shell_id":p.shell_id,"unchanged":true,"hash":hash});
                            // Additive: the phone tracks the scrollback's growth.
                            if let Some((size, alternate)) = scrollback_fields(&v) {
                                result["history_size"] = json!(size);
                                result["alternate"] = json!(alternate);
                            }
                            Ok(result)
                        }
                        _ => Err(cli_fault(
                            "CLI reported unchanged output that was not asked",
                        )),
                    };
                }
                let text = v
                    .get("output")
                    .and_then(Value::as_str)
                    .ok_or_else(|| cli_fault("missing CLI output"))?;
                if styled && !sgr_only(text) {
                    return Err(cli_fault(
                        "CLI returned escape sequences other than SGR for styled output",
                    ));
                }
                let mut result = json!({"shell_id":p.shell_id,"output":text});
                // Additive: the last `rows` lines of `output` are the visible
                // screen. Absent when the desktop could not read the pane.
                if let (Some(screen), Some(map)) = (screen_fields(&v, text), result.as_object_mut())
                    && let Some(fields) = screen.as_object()
                {
                    map.extend(fields.clone());
                }
                // Additive: the scrollback lines above the screen, and whether
                // a full-screen program is on the alternate screen.
                if let Some((size, alternate)) = scrollback_fields(&v) {
                    result["history_size"] = json!(size);
                    result["alternate"] = json!(alternate);
                }
                // Additive: names this exact answer, for `if_changed`.
                if let Some(hash) = hash {
                    result["hash"] = json!(hash);
                }
                Ok(result)
            }
            "shell.history" => {
                let p: History = params(r)?;
                let cap = self.history_max_lines();
                if !(1..=cap).contains(&p.lines) {
                    return Err(invalid(format!("lines must be 1..={cap}")));
                }
                let styled = p.styled.unwrap_or(false);
                let checked = self.ensure_selected(&p.shell_id).await?;
                let mut args: Vec<String> = [
                    "shell",
                    "history",
                    p.shell_id.as_str(),
                    "--end",
                    &p.end.to_string(),
                    "--lines",
                    &p.lines.to_string(),
                ]
                .into_iter()
                .map(String::from)
                .collect();
                if styled {
                    args.push("--styled".into());
                }
                // Never a wait, so the same limit as any other call.
                let v = match self.read_capped(args, CLI_TIMEOUT, reply_limit).await {
                    Ok(v) => v,
                    Err(fault) => {
                        let fault = history_fault(self.explain(checked, fault, &p.shell_id).await);
                        // An older CLI: remember what it takes, so the next page is not sent to it in vain.
                        if fault.code == "cli_error"
                            && let Some(limit) = cli_lines_limit(&fault.message)
                        {
                            self.history_cap.fetch_min(limit, Ordering::Relaxed);
                        }
                        return Err(fault);
                    }
                };
                history_result(&p.shell_id, &v, p.lines, styled).ok_or_else(|| {
                    cli_fault("CLI returned a history page that does not fit the request")
                })
            }
            "shell.resize" => {
                let p: Resize = params(r)?;
                id(&p.shell_id)?;
                if !(20..=300).contains(&p.columns) || !(8..=160).contains(&p.rows) {
                    return Err(invalid("columns must be 20..300 and rows 8..160"));
                }
                let mut held = viewport.hold().await;
                let v = held
                    .get()
                    .ok_or_else(|| invalid("authenticated connection required"))?;
                self.selected(&p.shell_id).await?;
                if v.selected
                    .as_ref()
                    .is_some_and(|(s, _, _)| *s != p.shell_id)
                {
                    self.clear_viewport(v).await.map_err(viewport_fault)?;
                }
                // Track before invoking CLI, so cancellation during the command
                // still clears any geometry it may have applied.
                v.selected = Some((p.shell_id.clone(), p.columns, p.rows));
                if let Err(e) = self.renew_viewport(v).await {
                    let fault = viewport_fault(e);
                    if fault.code == "viewport_busy" {
                        v.selected = None; // denied ownership must not be renewed
                    } else {
                        let _ = self.clear_viewport(v).await;
                    }
                    return Err(fault);
                }
                Ok(json!({"shell_id":p.shell_id,"columns":p.columns,"rows":p.rows}))
            }
            "shell.resize.clear" => {
                let p: Clear = params(r)?;
                id(&p.shell_id)?;
                let mut held = viewport.hold().await;
                let v = held
                    .get()
                    .ok_or_else(|| invalid("authenticated connection required"))?;
                // The CLI creates a lock file per shell ID it is asked about, so an
                // arbitrary UUID must not reach it. A shell this connection pinned
                // may have died since; clearing it must still work.
                if v.selected.as_ref().is_none_or(|(s, _, _)| *s != p.shell_id) {
                    self.selected(&p.shell_id).await?;
                }
                self.raw(v.args("resize-clear", &p.shell_id))
                    .await
                    .map_err(viewport_fault)?;
                if v.selected
                    .as_ref()
                    .is_some_and(|(s, _, _)| *s == p.shell_id)
                {
                    v.selected = None;
                }
                Ok(json!({"shell_id":p.shell_id,"status":"cleared"}))
            }
            "shell.input" => {
                let p: Input = params(r)?;
                id(&p.shell_id)?;
                if p.line.len() > 8192
                    || p.line
                        .chars()
                        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
                {
                    return Err(invalid(
                        "input must be one physical line <=8192 bytes without controls",
                    ));
                }
                self.input(device, r, p).await
            }
            "appearance.get" => {
                let _: Empty = params(r)?;
                self.appearance().await
            }
            "shell.keys" => {
                let p: Keys = params(r)?;
                id(&p.shell_id)?;
                id(&p.batch)?;
                let items = key_items(&p.items)?;
                self.keys(device, p, items).await
            }
            "shell.create" => {
                let spec = create_spec(&r.params)?;
                self.create(device, spec).await
            }
            "shell.close" => {
                let p: Close = params(r)?;
                id(&p.shell_id)?;
                self.close(device, &p.shell_id, viewport).await
            }
            "project.create" => {
                let spec = project_spec(&r.params)?;
                self.create_project(device, spec).await
            }
            "orchestrator.create" => {
                let project = orchestrator::spec(&r.params)?;
                self.orchestrator_create(device, project).await
            }
            // Chats; see `chat`.
            "chats.list" => {
                let spec = chat::list_spec(&r.params)?;
                self.chats_list(spec, reply_limit).await
            }
            "chat.create" => {
                let spec = chat::new_spec(&r.params)?;
                self.chat_create(device, spec).await
            }
            "chat.events" => {
                let spec = chat::events_spec(&r.params)?;
                self.chat_events(&r.id, spec, reply_limit).await
            }
            "chat.command" => {
                let spec = chat::command_spec(&r.params)?;
                self.chat_command(device, spec).await
            }
            "chat.stop" => {
                let spec = chat::stop_spec(&r.params)?;
                self.chat_stop(device, spec).await
            }
            // Files from the phone; see `upload`.
            "upload.begin" => {
                let spec = upload::begin_spec(r)?;
                self.upload_begin(device, spec).await
            }
            "upload.chunk" => {
                let chunk = upload::chunk_spec(r)?;
                self.upload_chunk(device, chunk).await
            }
            "upload.finish" => {
                let id = upload::one_spec(r)?;
                self.upload_finish(device, id).await
            }
            "upload.cancel" => {
                let id = upload::one_spec(r)?;
                self.upload_cancel(device, id).await
            }
            "shell.paste" => {
                let spec = upload::paste_spec(r)?;
                self.shell_paste(device, spec).await
            }
            // A desktop device's terminal streams; see `pty`.
            "pty.open" => {
                let set = pty_set(pty)?;
                let spec = pty::open_spec(&r.params)?;
                self.require_attach_exec().await?;
                Ok(pty::open(set, &self.cli, spec).await?)
            }
            "pty.read" => Ok(pty::read(pty_set(pty)?, &r.params).await?),
            "pty.write" | "pty.resize" | "pty.close" => {
                Ok(pty::inline(pty_set(pty)?, &r.method, &r.params)?)
            }
            _ => Err(invalid("unsupported RPC method")),
        }
    }
    /// Start a terminal in a project or worktree that exists on the desktop,
    /// exactly as `riwork shell create` does: the CLI writes the session down
    /// and starts it, and the open desktop app picks it up as a tab. Runs in
    /// the ordered lane, which a phone that drops does not cut short, and the
    /// CLI itself runs in a task of its own (see below). Not idempotent: a
    /// repeat starts another terminal.
    async fn create(&self, device: &str, spec: CreateSpec) -> std::result::Result<Value, Fault> {
        // Authorization was checked when the request started; this acts.
        if !self.storage.authorized(device).map_err(cli_fault)? {
            return Err(Fault::new("not_found", "device revoked"));
        }
        self.target_exists(&spec.target, create_fault).await?;
        // The CLI starts the tmux session and only then writes it into the
        // registry; a CLI killed in between leaves a session nobody can see or
        // close. The connection's tasks are dropped (and their CLI processes
        // killed) when it ends for any reason, revocation and relay errors
        // included, so the CLI runs in a task that outlives the request: if the
        // request is dropped, only the answer is lost.
        let runner = self.detached();
        let args = create_args(&spec);
        let created = tokio::spawn(async move { runner.read_within(args, CREATE_TIMEOUT).await })
            .await
            .map_err(|e| cli_fault(format!("creating the terminal was interrupted: {e}")))?
            .map_err(create_fault)?;
        create_result(&spec, &created).ok_or_else(|| {
            // Not what was asked for. It was just made, so end it rather than
            // leave a terminal the phone knows nothing about.
            if let Some(stray) = created.get("id").and_then(Value::as_str)
                && id(stray).is_ok()
                && created.get("kind").and_then(Value::as_str) == Some("project")
            {
                let (runner, stray) = (self.detached(), stray.to_owned());
                tokio::spawn(async move {
                    let _ = runner
                        .raw(vec!["shell".into(), "close".into(), stray])
                        .await;
                });
            }
            cli_fault("CLI returned a session that does not match the request")
        })
    }
    /// Make a new project in the desktop's default projects folder, as
    /// `riwork project create --name NAME --exclusive` does: the folder (a Git
    /// repository unless `git` is off) and the project. Nothing that is already
    /// there is touched; a taken name or folder is `already_exists`. Runs in the
    /// ordered lane, which a phone that drops does not cut short, and the CLI
    /// itself runs in a task of its own (see below). Not idempotent: a repeat
    /// finds the first and answers `already_exists`.
    async fn create_project(
        &self,
        device: &str,
        spec: ProjectSpec,
    ) -> std::result::Result<Value, Fault> {
        // Authorization was checked when the request started; this acts.
        if !self.storage.authorized(device).map_err(cli_fault)? {
            return Err(Fault::new("not_found", "device revoked"));
        }
        self.require_exclusive_project_create().await?;
        // The CLI makes the folder and only then writes the project down; a CLI
        // killed in between leaves a folder the next attempt would find taken.
        // The connection's tasks are dropped (and their CLI processes killed)
        // when it ends for any reason, so the CLI runs in a task that outlives
        // the request: if the request is dropped, only the answer is lost.
        let runner = self.detached();
        let args = project_args(&spec);
        let created = tokio::spawn(async move { runner.read_within(args, CREATE_TIMEOUT).await })
            .await
            .map_err(|e| cli_fault(format!("creating the project was interrupted: {e}")))?
            .map_err(|fault| project_create_fault(fault, &spec.name))?;
        project_create_result(&spec, &created)
            .ok_or_else(|| cli_fault("CLI returned a project that does not match the request"))
    }
    /// Whether the installed CLI knows `project create --exclusive` (`riwork
    /// capabilities`). A CLI that does not would read the flag as the project's
    /// PATH and make a folder of that name next to the connector, so it is never
    /// sent the flag. Asked every time, not remembered: a CLI updated while the
    /// connector runs is believed at once, and a creation is rare.
    async fn require_exclusive_project_create(&self) -> std::result::Result<(), Fault> {
        let too_old = || {
            Fault::new(
                "cli_error",
                "the installed riwork CLI cannot create projects from the phone; update RiWork",
            )
        };
        match self.raw(vec!["capabilities".into(), "--json".into()]).await {
            Ok(data) => {
                let reply = serde_json::from_slice::<Value>(&data).unwrap_or(Value::Null);
                if reply.get("v") == Some(&json!(1))
                    && reply.get("project_create_exclusive") == Some(&Value::Bool(true))
                {
                    Ok(())
                } else {
                    Err(too_old())
                }
            }
            // It ran and refused the question: a CLI from before capabilities.
            Err(e) if e.to_string().starts_with("RiWork CLI failed") => Err(too_old()),
            Err(e) => Err(cli_fault(e)),
        }
    }
    /// An `Rpc` for the same CLI and home, to run a call in a task that is not
    /// tied to the request that started it.
    fn detached(&self) -> Rpc {
        Rpc::new(self.cli.clone(), self.storage.clone())
    }
    /// The project or worktree must exist under exactly this id. The CLI also
    /// matches names, branches, paths and id prefixes, so an id that is nobody's
    /// could otherwise start a terminal somewhere else.
    async fn target_exists(
        &self,
        target: &CreateTarget,
        explain: fn(Fault) -> Fault,
    ) -> std::result::Result<(), Fault> {
        let (kind, target) = match target {
            CreateTarget::Project(project) => ("project", project),
            CreateTarget::Worktree(worktree) => ("worktree", worktree),
        };
        let shown = self.read(&[kind, "show", target]).await.map_err(explain)?;
        if shown.get("id").and_then(Value::as_str) == Some(target) {
            Ok(())
        } else {
            Err(Fault::new(
                "not_found",
                format!("{kind} not found on the desktop"),
            ))
        }
    }
    /// End a project terminal and its process. Orchestrators are not closed
    /// from the phone. A terminal that already exited can be closed too, which
    /// takes it off the desktop's list.
    async fn close(
        &self,
        device: &str,
        shell: &str,
        viewport: ViewportAccess<'_>,
    ) -> std::result::Result<Value, Fault> {
        // Typing into this shell, from any device, finishes first.
        let lock = self.input_lock(shell);
        let _guard = lock.lock().await;
        let Some(found) = self.session(shell).await? else {
            return Err(Fault::new("not_found", SHELL_NOT_FOUND));
        };
        if found.get("kind").and_then(Value::as_str) != Some("project") {
            return Err(invalid("only a project terminal can be closed"));
        }
        if !self.storage.authorized(device).map_err(cli_fault)? {
            return Err(Fault::new("not_found", "device revoked"));
        }
        {
            // This connection's own resize override ends with the shell.
            let mut held = viewport.hold().await;
            if let Some(v) = held.get()
                && v.selected.as_ref().is_some_and(|(s, _, _)| s == shell)
            {
                let _ = self.clear_viewport(v).await;
                // The shell is about to go. A lease that could not be released
                // lapses by itself; keeping it would only fail to renew and
                // end the connection.
                v.selected = None;
            }
        }
        self.raw(vec!["shell".into(), "close".into(), shell.into()])
            .await
            .map_err(|e| close_fault(cli_fault(e)))?;
        Ok(json!({"shell_id":shell,"status":"closed"}))
    }
    /// The colors the desktop published for the phone: read-only, no shell.
    async fn appearance(&self) -> std::result::Result<Value, Fault> {
        let data = self
            .raw(vec!["appearance".into(), "--json".into()])
            .await
            .map_err(|e| appearance_fault(&e))?;
        appearance::validate(&data).ok_or_else(appearance_not_published)
    }
    pub async fn renew_viewport(&self, v: &Viewport) -> Result<()> {
        if let Some((shell, columns, rows)) = &v.selected {
            let mut args = v.args("resize", shell);
            args.extend([
                "--columns".into(),
                columns.to_string(),
                "--rows".into(),
                rows.to_string(),
            ]);
            self.raw(args).await?;
        }
        Ok(())
    }
    pub async fn clear_viewport(&self, v: &mut Viewport) -> Result<()> {
        if let Some((shell, _, _)) = &v.selected {
            self.raw(v.args("resize-clear", shell)).await?;
        }
        v.selected = None;
        Ok(())
    }
    async fn input(
        &self,
        device: &str,
        r: &Request,
        p: Input,
    ) -> std::result::Result<Value, Fault> {
        id(device)?;
        // Requests of one device run concurrently, but the file lock below is
        // a try-lock: without this, a second input would fail instead of wait.
        let device_lock = self.input_lock(&format!("input-{device}"));
        let _device_guard = device_lock.lock().await;
        // Only one connector per home; this lock also
        // protects external recovery tooling and tests from racing input writes.
        let _lock = self
            .storage
            .lock(&format!("outcomes-{device}.lock"))
            .map_err(cli_fault)?;
        let path = self.storage.dir.join(format!("outcomes-{device}.json"));
        let mut ledger: Ledger = if path.exists() {
            private_read(&path, 64 * 1024 * 1024).map_err(cli_fault)?
        } else {
            Ledger::default()
        };
        let digest = hex::encode(Sha256::digest(serde_json::to_vec(r).map_err(cli_fault)?));
        if let Some(record) = ledger.entries.get(&r.id) {
            if record.digest != digest {
                return Err(Fault::new(
                    "request_conflict",
                    "input request UUID already used with different contents",
                ));
            }
            if let Some(response) = &record.response {
                if response.get("ok") == Some(&Value::Bool(true)) {
                    return Ok(response["result"].clone());
                }
                return Err(Fault::new(
                    "outcome_unknown",
                    response.get("error").and_then(|e| e.get("message")).and_then(Value::as_str)
                        .unwrap_or("previous send outcome is unknown; inspect the selected shell before any manual retry"),
                ));
            }
            return Err(Fault::new(
                "outcome_unknown",
                "pending send outcome is unknown; never automatically resend with a new UUID",
            ));
        }
        if ledger.entries.len() >= 4096 {
            return Err(Fault::new(
                "cache_full",
                "input outcome cache full; review outcomes then pair a new device",
            ));
        }
        // Await per-shell serialization across every paired device. The root
        // CLI also locks the complete paste/Return transaction across processes.
        let shell_lock = self.input_lock(&p.shell_id);
        let _shell_guard = shell_lock.lock().await;
        self.selected(&p.shell_id).await?;
        if !self.storage.authorized(device).map_err(cli_fault)? {
            return Err(Fault::new("not_found", "device revoked"));
        }
        ledger.entries.insert(
            r.id.clone(),
            Record {
                digest,
                response: None,
            },
        );
        private_write(&path, &ledger).map_err(cli_fault)?; // durable before irreversible submission
        let sent = self
            .raw(vec![
                "shell".into(),
                "send".into(),
                p.shell_id.clone(),
                p.line,
            ])
            .await;
        let result = match sent {
            Ok(_) => Ok(json!({"shell_id":p.shell_id,"status":"sent"})),
            Err(_) => Err(Fault::new(
                "outcome_unknown",
                "CLI submission outcome unknown; inspect selected shell; do not automatically resend",
            )),
        };
        let response = match &result {
            Ok(v) => success(&r.id, v.clone()),
            Err(f) => error(&r.id, f.code, &f.message),
        };
        ledger
            .entries
            .get_mut(&r.id)
            .expect("inserted record")
            .response = Some(response);
        if private_write(&path, &ledger).is_err() {
            return Err(Fault::new(
                "outcome_unknown",
                "submission attempted but outcome persistence failed; do not resend",
            ));
        }
        result
    }
    /// Type one batch into a shell, exactly once per (device, batch UUID).
    async fn keys(
        &self,
        device: &str,
        p: Keys,
        items: Vec<String>,
    ) -> std::result::Result<Value, Fault> {
        id(device)?;
        // One batch at a time per device, so a retry that overlaps its own
        // first attempt waits for it and then reports `duplicate`. The file
        // lock keeps external recovery tooling out of the ledger meanwhile.
        let device_lock = self.input_lock(&format!("keys-{device}"));
        let _device_guard = device_lock.lock().await;
        let _lock = self
            .storage
            .lock(&format!("keys-{device}.lock"))
            .map_err(cli_fault)?;
        let path = self.storage.dir.join(format!("keys-{device}.json"));
        let mut ledger: BatchLedger = if path.exists() {
            private_read(&path, 8 * 1024 * 1024).map_err(cli_fault)?
        } else {
            BatchLedger::default()
        };
        let reply =
            |status: &str| Ok(json!({"shell_id":p.shell_id,"batch":p.batch,"status":status}));
        // A repeat is answered from the ledger alone, before any CLI runs.
        if let Some(record) = ledger.batches.iter().find(|r| r.batch == p.batch) {
            return reply(match record.state {
                BatchState::Sent => "duplicate",
                BatchState::Pending => "uncertain",
            });
        }
        let shell_lock = self.input_lock(&p.shell_id);
        let _shell_guard = shell_lock.lock().await;
        let checked = self.ensure_selected(&p.shell_id).await?;
        if !self.storage.authorized(device).map_err(cli_fault)? {
            return Err(Fault::new("not_found", "device revoked"));
        }
        let excess = (ledger.batches.len() + 1).saturating_sub(KEYS_LEDGER_MAX);
        ledger.batches.drain(..excess);
        ledger.batches.push(BatchRecord {
            batch: p.batch.clone(),
            state: BatchState::Pending,
        });
        private_write(&path, &ledger).map_err(cli_fault)?; // durable before typing
        let mut args: Vec<String> = ["shell", "keys", p.shell_id.as_str(), "--"]
            .into_iter()
            .map(String::from)
            .collect();
        args.extend(items);
        match self.raw(args).await {
            Ok(_) => {
                if let Some(record) = ledger.batches.last_mut() {
                    record.state = BatchState::Sent;
                }
                // The keys are typed either way; if this write fails the batch
                // stays pending and a repeat reports uncertain, never a resend.
                // It is not worth a second directory sync (about half of what
                // the write costs, and the phone waits for this answer before
                // it sends the next keys): a crash that loses the rename leaves
                // the pending record, which says exactly that.
                let _ = private_write_relaxed(&path, &ledger);
                reply("sent")
            }
            Err(error) => {
                let (fault, not_sent) = keys_fault(&error);
                if not_sent {
                    // Nothing was typed: forget the batch so a retry can send it.
                    ledger.batches.pop();
                    let _ = private_write(&path, &ledger);
                }
                // After the ledger is right again: this may ask the CLI twice more.
                Err(self.explain(checked, fault, &p.shell_id).await)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_are_one_visible_folder_name_within_the_limits() {
        for good in ["a", "My App", "my--app", "app.", "\u{e9}", &"x".repeat(100)] {
            assert!(project_name(good).is_ok(), "{good:?}");
        }
        // 255 bytes exactly, in three-byte characters: 85 of them.
        assert!(project_name(&"\u{65e5}".repeat(85)).is_ok());
        for bad in [
            "",
            " ",
            " a",
            "a ",
            "a\u{a0}",
            ".",
            "..",
            ".a",
            "a/b",
            "a\\b",
            "a\u{0}b",
            "a\tb",
            "a\u{2028}b",
            "-a",
            "--json",
            &"x".repeat(101),
            &"\u{65e5}".repeat(86),
        ] {
            let fault = project_name(bad).unwrap_err();
            assert_eq!(fault.code, "invalid_request", "{bad:?}");
        }
    }

    #[test]
    fn a_creation_the_connector_stopped_is_told_apart_from_one_that_failed() {
        let stopped = project_create_fault(cli_fault("RiWork CLI timeout"), "Fresh");
        assert_eq!(stopped.code, "cli_error");
        assert!(
            stopped
                .message
                .contains("check the project list before trying again"),
            "{}",
            stopped.message
        );
        // A fault that is not a CLI failure passes through untouched.
        let big = project_create_fault(Fault::new("response_too_large", "x"), "Fresh");
        assert_eq!(
            (big.code, big.message.as_str()),
            ("response_too_large", "x")
        );
    }

    #[test]
    fn a_created_shell_passes_on_its_mode_but_not_a_chat_it_does_not_have() {
        let (project_id, shell) = (
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        let spec = CreateSpec {
            target: CreateTarget::Project(project_id.clone()),
            kind: CreateKind::Shell,
            unrestricted: false,
            command: None,
        };
        let mut cli = json!({
            "id": shell, "project_id": project_id, "worktree_id": null, "kind": "project",
            "cwd": "/work", "command": null, "harness": null, "alive": true,
            "created_at_unix": 1790000000u64, "mode": "terminal"
        });
        let result = create_result(&spec, &cli).unwrap();
        assert_eq!(result["shell"]["mode"], "terminal");
        // The fields of a chat go with `mode` "chat" alone.
        cli["chat_id"] = json!(shell);
        cli["provider"] = json!("codex");
        let shown = create_result(&spec, &cli).unwrap();
        assert!(shown["shell"].get("chat_id").is_none());
        assert!(shown["shell"].get("provider").is_none());
        // An older CLI says none of it, and the result is what it was.
        for field in ["mode", "chat_id", "provider"] {
            cli.as_object_mut().unwrap().remove(field);
        }
        assert_eq!(
            create_result(&spec, &cli).unwrap()["shell"],
            json!({
                "id": shell, "project_id": project_id, "worktree_id": null, "kind": "project",
                "cwd": "/work", "harness": null, "alive": true, "created_at_unix": 1790000000u64
            })
        );
    }

    #[test]
    fn a_waiting_cli_gets_the_wait_plus_a_margin_and_never_less_than_other_calls() {
        assert_eq!(cli_limit(0), Duration::from_secs(15));
        assert_eq!(cli_limit(5_000), Duration::from_secs(15));
        assert_eq!(cli_limit(7_000), Duration::from_secs(15));
        assert_eq!(cli_limit(8_000), Duration::from_secs(16));
        assert_eq!(cli_limit(MAX_WAIT_MS), Duration::from_secs(18));
        // The longest allowed wait leaves at least the margin (two tmux
        // calls' worth of slack) before the connector gives up on the CLI.
        assert!(
            cli_limit(MAX_WAIT_MS)
                >= Duration::from_millis(MAX_WAIT_MS as u64) + Duration::from_secs(5)
        );
        // Out of range never widens the limit.
        assert_eq!(cli_limit(i64::MAX), cli_limit(MAX_WAIT_MS));
        assert_eq!(cli_limit(-1), Duration::from_secs(15));
    }

    #[test]
    fn hashes_are_short_and_printable() {
        for good in ["0123456789abcdef", "a", &"x".repeat(64), "A-b_c.d~"] {
            assert!(hash_shaped(good), "{good}");
        }
        for bad in [
            "",
            &"x".repeat(65),
            "a b",
            "a\tb",
            "a\nb",
            "\u{e9}",
            "a\u{7f}",
            "a\u{1b}",
        ] {
            assert!(!hash_shaped(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_well_formed_sgr_passes_the_styled_check() {
        for good in [
            "",
            "plain\n\ttext \u{e9}",
            "\u{1b}[m",
            "\u{1b}[0m",
            "\u{1b}[1;31mred\u{1b}[0m\nnext",
            "\u{1b}[38;5;200m\u{1b}[38;2;1;2;3m\u{1b}[38:2::1:2:3m",
            &format!("\u{1b}[{}m", "1".repeat(64)),
        ] {
            assert!(sgr_only(good), "{good:?}");
        }
        for bad in [
            "\u{1b}",
            "\u{1b}[",
            "\u{1b}[31",
            "\u{1b}[2J",
            "\u{1b}[?25l",
            "\u{1b}[>4;2m",
            "\u{1b}[1 m",
            "\u{1b}]0;title\u{7}",
            "\u{1b}]8;;u\u{1b}\\",
            "\u{1b}(0",
            "\u{1b}7",
            "\u{1b}[31\u{1b}[0m",
            &format!("\u{1b}[{}m", "1".repeat(65)),
            "a\rb",
            "a\u{7}b",
            "a\u{e}b",
            "a\u{0}b",
            "a\u{7f}b",
            "a\u{9b}31mb",
            "a\u{85}b",
        ] {
            assert!(!sgr_only(bad), "{bad:?}");
        }
    }
}
