//! Chats on the phone ("Chat extension" in `docs/remote-protocol.md`): `chats.list`,
//! `chat.create`, `chat.events`, `chat.command`, `chat.stop`, `chat.models` and
//! `chat.suggestions`.
//!
//! Each is one call of the installed CLI (`riwork chat ...`), which talks to the chat host.
//! The connector validates the params before any CLI runs, builds the argument vector from
//! the validated values (one argument per value, never a shell string), and checks what the
//! CLI answers before it reaches the phone. The chat JSON itself (`ChatInfo`, `ChatEvent`,
//! `ChatCommand`) is defined by `riwork_engine::chat::model` and is passed on as the CLI printed
//! it: the phone decodes it leniently.
//!
//! - `chat.events` is a long poll. The CLI waits for the first event after `since`, collects
//!   what follows within a short window and prints a page; the connector cuts the page until
//!   its reply fits one encrypted frame, deflated if the session opted in.
//! - `chat.create` changes what exists, so like `shell.create` it runs in a task that
//!   outlives the request.
//! - `chat.suggestions` passes on a listing of the chat's folder, cut until its reply fits one
//!   encrypted frame, as `chat.events` cuts a page.
use super::*;
use crate::{MAX_PLAINTEXT, link};
use serde_json::Map;

/// The longest `Send.text` in bytes.
pub const TEXT_MAX: usize = 64 * 1024;
/// The longest `model`, `effort` and `title` of a new chat, and the same limits for a
/// `Configure` command, in characters (Unicode scalar values).
pub const MODEL_MAX_CHARS: usize = 100;
pub const EFFORT_MAX_CHARS: usize = 32;
pub const TITLE_MAX_CHARS: usize = 200;
/// The longest slash command name `chat.suggestions` passes on, in bytes (`[a-z0-9-]`).
const SUGGESTED_COMMAND_MAX: usize = 40;
/// The most `&` models `chat.suggestions` passes on, and the longest name or description of
/// one, in characters. A model's id is a `Configure` model (`MODEL_MAX_CHARS`).
const SUGGESTED_MODELS_MAX: usize = 200;
const SUGGESTED_MODEL_TEXT_MAX: usize = 500;
/// The longest provider request id an approval or an answer may name.
const REQUEST_ID_MAX_BYTES: usize = 200;
/// What an `Answer` may hold: questions, choices per question, bytes of one choice, bytes in all.
const ANSWER_QUESTIONS_MAX: usize = 16;
const ANSWER_CHOICES_MAX: usize = 64;
const ANSWER_BYTES_MAX: usize = 8 * 1024;
const ANSWER_TOTAL_MAX: usize = TEXT_MAX;
/// The longest `chat.events` may wait for a first event.
pub const EVENTS_WAIT_MS_MAX: i64 = 25_000;
/// Events one `chat.events` page may hold, and what it holds when `max_events` is left out.
pub const EVENTS_MAX: u64 = 2000;
pub const EVENTS_DEFAULT: u64 = 500;
/// What a reply holds besides events (the envelope, the ids, `next`, `more` and `server_ms`),
/// with room to spare: the CLI gets the reply limit less this as the size of its page.
const REPLY_SLACK: usize = 1024;
/// Bounded CLI input allowance, independent of the sealed reply size. Large original
/// events must reach recovery before a transport-size check can reject them.
const CHAT_READ_MAX: usize = 8 * 1024 * 1024;
/// A chat is started (and its agent with it), or resumed by a message: the same allowance
/// as starting a terminal.
const COMMAND_TIMEOUT: Duration = CREATE_TIMEOUT;

/// The values of `approval_mode`: the wire name and the CLI's word for it.
const MODES: [(&str, &str); 4] = [
    ("supervised", "supervised"),
    ("auto_edit", "auto-edit"),
    ("full", "full"),
    ("plan", "plan"),
];
const DECISIONS: [&str; 4] = ["accept", "accept_for_session", "decline", "cancel"];

// ---- Params ------------------------------------------------------------------------------------

/// The params object with no field outside `allowed`.
fn fields<'a>(
    params: &'a Value,
    allowed: &[&str],
) -> std::result::Result<&'a Map<String, Value>, Fault> {
    let object = params
        .as_object()
        .ok_or_else(|| invalid("params must be an object"))?;
    if let Some(unknown) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(invalid(format!("unknown field {unknown}")));
    }
    Ok(object)
}
/// A string field: absent is `None`; null and every other type are refused.
fn text<'a>(
    object: &'a Map<String, Value>,
    name: &str,
) -> std::result::Result<Option<&'a str>, Fault> {
    match object.get(name) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(invalid(format!("{name} must be a string"))),
    }
}
/// Muse's contributor mode a request asks for: `contributor_mode` on or off, or the off a phone
/// from before it says with `contributor_opt_out` true. That field never asks for on; its false
/// asked for the Muse settings, which a chat no longer follows: on `chat.create` it is the off
/// every chat starts with, on `configure` (`settings_gone`) it is refused. On beside an off is
/// refused.
fn contributor(
    object: &Map<String, Value>,
    settings_gone: bool,
) -> std::result::Result<Option<&'static str>, Fault> {
    let mode = match text(object, "contributor_mode")? {
        None => None,
        Some("on") => Some("on"),
        Some("off") => Some("off"),
        Some(_) => return Err(invalid("contributor_mode must be on or off")),
    };
    match (mode, flag(object, "contributor_opt_out")?) {
        (Some("on"), Some(true)) => Err(invalid(
            "contributor_mode on cannot be combined with contributor_opt_out, which asks for off",
        )),
        (_, Some(false)) if settings_gone => Err(invalid(
            "contributor_opt_out false asked for the Muse settings, which a chat no longer follows; send contributor_mode on or off",
        )),
        (None, Some(true)) => Ok(Some("off")),
        (mode, _) => Ok(mode),
    }
}
/// A boolean field: absent is `None`; null and every other type are refused.
fn flag(object: &Map<String, Value>, name: &str) -> std::result::Result<Option<bool>, Fault> {
    match object.get(name) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(invalid(format!("{name} must be a boolean"))),
    }
}
fn required<'a>(object: &'a Map<String, Value>, name: &str) -> std::result::Result<&'a str, Fault> {
    text(object, name)?.ok_or_else(|| invalid(format!("{name} is required")))
}
/// A full lowercase canonical UUID: never a name, a path or a prefix.
fn uuid_field(
    object: &Map<String, Value>,
    name: &str,
) -> std::result::Result<Option<String>, Fault> {
    match text(object, name)? {
        None => Ok(None),
        Some(value) => {
            id(value).map_err(|_| invalid(format!("{name} must be a full lowercase UUID")))?;
            Ok(Some(value.to_owned()))
        }
    }
}
fn chat_id(object: &Map<String, Value>) -> std::result::Result<String, Fault> {
    uuid_field(object, "chat_id")?.ok_or_else(|| invalid("chat_id is required"))
}
/// A non-negative integer field; a float, a negative number or a string is refused.
fn number(object: &Map<String, Value>, name: &str) -> std::result::Result<Option<u64>, Fault> {
    match object.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid(format!("{name} must be a non-negative integer"))),
    }
}
fn has_control(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
}
/// A model, an effort or a title: at most `max` characters, one line. Whitespace only is the
/// same as leaving it out (`None`), as the desktop treats a blank title.
fn label(
    name: &str,
    value: Option<&str>,
    max: usize,
) -> std::result::Result<Option<String>, Fault> {
    let Some(value) = value else { return Ok(None) };
    if value.chars().count() > max {
        return Err(invalid(format!("{name} must be at most {max} characters")));
    }
    if has_control(value) {
        return Err(invalid(format!(
            "{name} must not contain control characters"
        )));
    }
    Ok((!value.trim().is_empty()).then(|| value.to_owned()))
}
/// The wire name of an `approval_mode`, checked against the four there are.
fn mode(value: &str) -> std::result::Result<&'static str, Fault> {
    MODES
        .iter()
        .find(|(wire, _)| *wire == value)
        .map(|(wire, _)| *wire)
        .ok_or_else(|| invalid("approval_mode must be supervised, auto_edit, full or plan"))
}

/// A validated `chats.list`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ListSpec {
    project: Option<String>,
}
pub(super) fn list_spec(params: &Value) -> std::result::Result<ListSpec, Fault> {
    let object = fields(params, &["project_id"])?;
    Ok(ListSpec {
        project: uuid_field(object, "project_id")?,
    })
}

/// A validated `chat.create`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct NewSpec {
    provider: &'static str,
    target: CreateTarget,
    mode: &'static str,
    model: Option<String>,
    effort: Option<String>,
    fast: bool,
    /// Muse's contributor mode on; off, as every chat starts, without it.
    contributor_on: bool,
    title: Option<String>,
}
pub(super) fn new_spec(params: &Value) -> std::result::Result<NewSpec, Fault> {
    let object = fields(
        params,
        &[
            "provider",
            "project_id",
            "worktree_id",
            "approval_mode",
            "model",
            "effort",
            "fast",
            "contributor_mode",
            "contributor_opt_out",
            "title",
        ],
    )?;
    let provider = match required(object, "provider")? {
        "codex" => "codex",
        "claude" => "claude",
        "muse" => "muse",
        _ => return Err(invalid("provider must be codex, claude or muse")),
    };
    let target = match (
        uuid_field(object, "project_id")?,
        uuid_field(object, "worktree_id")?,
    ) {
        (Some(project), None) => CreateTarget::Project(project),
        (None, Some(worktree)) => CreateTarget::Worktree(worktree),
        _ => return Err(invalid("give exactly one of project_id and worktree_id")),
    };
    let contributor_on = contributor(object, false)? == Some("on");
    if contributor_on && provider != "muse" {
        return Err(invalid(
            "contributor_mode on is for a Muse chat: only Muse has a contributor mode",
        ));
    }
    Ok(NewSpec {
        provider,
        target,
        mode: text(object, "approval_mode")?.map_or(Ok("supervised"), mode)?,
        model: label("model", text(object, "model")?, MODEL_MAX_CHARS)?,
        effort: label("effort", text(object, "effort")?, EFFORT_MAX_CHARS)?,
        fast: flag(object, "fast")?.unwrap_or(false),
        contributor_on,
        title: label("title", text(object, "title")?, TITLE_MAX_CHARS)?,
    })
}
/// The CLI's argv for a validated `chat.create`. `--json` is added by `read`. The three
/// free-text values use the `--name=VALUE` form, so that one that begins with `-` stays a
/// value.
fn new_args(spec: &NewSpec) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "chat".into(),
        "new".into(),
        "--provider".into(),
        spec.provider.into(),
    ];
    match &spec.target {
        CreateTarget::Project(project) => args.extend(["--project".into(), project.clone()]),
        CreateTarget::Worktree(worktree) => args.extend(["--worktree".into(), worktree.clone()]),
    }
    let cli_mode = MODES
        .iter()
        .find(|(wire, _)| *wire == spec.mode)
        .map_or("supervised", |(_, cli)| *cli);
    args.extend(["--mode".into(), cli_mode.into()]);
    for (name, value) in [
        ("model", &spec.model),
        ("effort", &spec.effort),
        ("title", &spec.title),
    ] {
        if let Some(value) = value {
            args.push(format!("--{name}={value}"));
        }
    }
    if spec.fast {
        args.push("--fast".into());
    }
    if spec.contributor_on {
        args.push("--contributor-on".into());
    }
    args
}

/// Additive event variants require an explicit decoder capability from the phone.
fn supports_rate_limits(object: &Map<String, Value>) -> std::result::Result<bool, Fault> {
    let Some(features) = object.get("features") else {
        return Ok(false);
    };
    let features = features
        .as_array()
        .filter(|v| v.len() <= 16)
        .ok_or_else(|| invalid("features must be an array of at most 16 strings"))?;
    let mut supported = false;
    for feature in features {
        let feature = feature
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 64)
            .ok_or_else(|| invalid("features must contain nonempty strings of at most 64 bytes"))?;
        supported |= feature == "rate_limits";
    }
    Ok(supported)
}

fn negotiate_snapshot_rate_limits(snapshot: &mut Value, supported: bool) {
    if let Some(controls) = snapshot["controls"].as_array_mut() {
        controls.retain(|event| {
            event["event"] != "rate_limits"
                || (supported && event["windows"].as_array().is_some_and(|w| !w.is_empty()))
        });
    }
}

/// Read-only snapshot/history. Opaque cursor pins the prefix, before is an item order.
pub(super) struct SnapshotSpec {
    chat: String,
    cursor: Option<String>,
    before: u64,
    limit: u64,
    items: Vec<String>,
    rate_limits: bool,
}
pub(super) fn snapshot_spec(params: &Value) -> std::result::Result<SnapshotSpec, Fault> {
    let object = fields(
        params,
        &[
            "chat_id", "cursor", "before", "limit", "item_ids", "features",
        ],
    )?;
    let cursor = text(object, "cursor")?.map(str::to_owned);
    if cursor.as_ref().is_some_and(|c| {
        c.len() > 80 || c.is_empty() || !c.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
    }) {
        return Err(invalid("invalid snapshot cursor"));
    }
    let items: Vec<String> = match object.get("item_ids") {
        None => Vec::new(),
        Some(value) => {
            serde_json::from_value(value.clone()).map_err(|_| invalid("invalid item ids"))?
        }
    };
    if items.len() > 100
        || items.iter().any(|id| id.is_empty() || id.len() > 512)
        || (!items.is_empty() && cursor.is_none())
    {
        return Err(invalid("invalid snapshot item ids"));
    }
    let before = number(object, "before")?.unwrap_or(u64::MAX);
    let limit = number(object, "limit")?.unwrap_or(50);
    if !(1..=100).contains(&limit) || (cursor.is_none() && before != u64::MAX) {
        return Err(invalid("invalid snapshot range"));
    }
    Ok(SnapshotSpec {
        chat: chat_id(object)?,
        cursor,
        before,
        limit,
        items,
        rate_limits: supports_rate_limits(object)?,
    })
}

/// A validated `chat.events`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct EventsSpec {
    chat: String,
    since: u64,
    wait_ms: i64,
    max_events: u64,
    complete: bool,
    bounded: bool,
    rate_limits: bool,
}
pub(super) fn events_spec(params: &Value) -> std::result::Result<EventsSpec, Fault> {
    let object = fields(
        params,
        &[
            "chat_id",
            "since",
            "wait_ms",
            "max_events",
            "complete",
            "bounded",
            "features",
        ],
    )?;
    let chat = chat_id(object)?;
    let since = number(object, "since")?.ok_or_else(|| invalid("since is required"))?;
    let wait_ms = number(object, "wait_ms")?.ok_or_else(|| invalid("wait_ms is required"))?;
    if wait_ms > EVENTS_WAIT_MS_MAX as u64 {
        return Err(invalid(format!("wait_ms must be 0..{EVENTS_WAIT_MS_MAX}")));
    }
    let max_events = number(object, "max_events")?.unwrap_or(EVENTS_DEFAULT);
    if !(1..=EVENTS_MAX).contains(&max_events) {
        return Err(invalid(format!("max_events must be 1..{EVENTS_MAX}")));
    }
    if flag(object, "complete")? == Some(true) && flag(object, "bounded")? == Some(true) {
        return Err(invalid("complete and bounded are exclusive"));
    }
    Ok(EventsSpec {
        chat,
        since,
        wait_ms: wait_ms as i64,
        max_events,
        complete: flag(object, "complete")?.unwrap_or(false),
        bounded: flag(object, "bounded")?.unwrap_or(false),
        rate_limits: supports_rate_limits(object)?,
    })
}
/// A validated `chat.models`: the provider whose models are wanted, and the project whose
/// Codex account the list is for.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ModelsSpec {
    provider: &'static str,
    project: Option<String>,
}
pub(super) fn models_spec(params: &Value) -> std::result::Result<ModelsSpec, Fault> {
    let object = fields(params, &["provider", "project_id"])?;
    let provider = match required(object, "provider")? {
        "codex" => "codex",
        "claude" => "claude",
        "muse" => "muse",
        _ => return Err(invalid("provider must be codex, claude or muse")),
    };
    Ok(ModelsSpec {
        provider,
        project: uuid_field(object, "project_id")?,
    })
}
/// The `chat.models` result for what the CLI printed, or `None` if it is not an answer for
/// `provider`: the models (each with an `id` and a `name`), the other ids chats were set to,
/// the Codex account's label and why the provider cannot be offered, and nothing else.
fn models_result(provider: &str, cli: &Value) -> Option<Value> {
    let models = cli.get("models")?.as_array()?;
    let model_ok = |model: &Value| {
        model.is_object()
            && model.get("id").is_some_and(Value::is_string)
            && model.get("name").is_some_and(Value::is_string)
    };
    let configured = cli.get("configured")?.as_array()?;
    let nullable_text = |name: &str| match cli.get(name) {
        None | Some(Value::Null) => Some(Value::Null),
        Some(Value::String(text)) => Some(json!(text)),
        Some(_) => None,
    };
    (cli.get("provider").and_then(Value::as_str) == Some(provider)
        && models.iter().all(model_ok)
        && configured.iter().all(Value::is_string))
    .then(|| {
        Some(json!({
            "provider": provider,
            "models": models,
            "configured": configured,
            "account_label": nullable_text("account_label")?,
            "error": nullable_text("error")?,
        }))
    })
    .flatten()
}

/// A validated `chat.suggestions`: the chat whose commands and files are wanted.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct SuggestionsSpec {
    chat: String,
}
pub(super) fn suggestions_spec(params: &Value) -> std::result::Result<SuggestionsSpec, Fault> {
    let object = fields(params, &["chat_id"])?;
    Ok(SuggestionsSpec {
        chat: chat_id(object)?,
    })
}
/// The `chat.suggestions` result for what the CLI printed, or `None` if it is not an answer for
/// `chat`: the chat's id, provider and folder, its commands, the files and folders of its folder,
/// the bound of a listing and its provider's models for `&`, and nothing else. A command, an
/// entry or a model that is malformed is left out on its own (see `suggested_command`,
/// `suggested_file`, `suggested_model`). A CLI from before `&` prints no models: the phone gets
/// none and offers no `&` list.
fn suggestions_result(chat: &str, cli: &Value) -> Option<Value> {
    if cli.get("chat_id")?.as_str()? != chat {
        return None;
    }
    let provider = cli
        .get("provider")?
        .as_str()
        .filter(|provider| ["codex", "claude", "muse"].contains(provider))?;
    let commands = cli.get("commands")?.as_array()?;
    let files = cli.get("files")?;
    let entries = files.get("entries")?.as_array()?;
    let models: Vec<Value> = match cli.get("models") {
        None | Some(Value::Null) => Vec::new(),
        Some(models) => models
            .as_array()?
            .iter()
            .filter_map(suggested_model)
            .take(SUGGESTED_MODELS_MAX)
            .collect(),
    };
    Some(json!({
        "chat_id": chat,
        "provider": provider,
        "root": cli.get("root")?.as_str()?,
        "commands": commands.iter().filter_map(suggested_command).collect::<Vec<_>>(),
        "files": {
            "entries": entries.iter().filter_map(suggested_file).collect::<Vec<_>>(),
            "truncated": files.get("truncated")?.as_bool()?,
        },
        "file_bound": cli.get("file_bound")?.as_u64()?,
        "models": models,
    }))
}
/// A model as the phone may offer it for `&`, which writes its id into the draft: an id a chat
/// may be set to (one line of at most `MODEL_MAX_CHARS`, not blank) and that reads as one word
/// in a mention (no blank space inside), a one-line name of at most `SUGGESTED_MODEL_TEXT_MAX`
/// characters, its description when that is such a line too (else `""`, rather than leave the
/// model out), whether it is the provider's default, and how many chats use it.
fn suggested_model(model: &Value) -> Option<Value> {
    let line = |text: &str, max: usize| text.chars().count() <= max && !has_control(text);
    let id = model.get("id")?.as_str().filter(|id| {
        let word = id.trim();
        !word.is_empty() && !word.contains(char::is_whitespace) && line(id, MODEL_MAX_CHARS)
    })?;
    let name = model
        .get("name")?
        .as_str()
        .filter(|name| line(name, SUGGESTED_MODEL_TEXT_MAX))?;
    let description = match model.get("description") {
        None | Some(Value::Null) => "",
        Some(Value::String(text)) if line(text, SUGGESTED_MODEL_TEXT_MAX) => text,
        Some(_) => "",
    };
    let is_default = match model.get("is_default") {
        None | Some(Value::Null) => false,
        Some(value) => value.as_bool()?,
    };
    let chats = match model.get("chats") {
        None | Some(Value::Null) => 0,
        Some(value) => value.as_u64()?,
    };
    Some(json!({
        "id": id,
        "name": name,
        "description": description,
        "is_default": is_default,
        "chats": chats,
    }))
}
/// A slash command as the phone may show it: a name of 1 to `SUGGESTED_COMMAND_MAX` of
/// `[a-z0-9-]`, a summary, an argument or null, what sending it does and who acts on it.
fn suggested_command(command: &Value) -> Option<Value> {
    let name = command.get("name")?.as_str().filter(|name| {
        (1..=SUGGESTED_COMMAND_MAX).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })?;
    let argument = match command.get("argument") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(argument)) => json!(argument),
        Some(_) => return None,
    };
    Some(json!({
        "name": name,
        "summary": command.get("summary")?.as_str()?,
        "argument": argument,
        "sends": command.get("sends")?.as_str()?,
        "handler": command
            .get("handler")?
            .as_str()
            .filter(|handler| ["provider", "riwork"].contains(handler))?,
    }))
}
/// A file or folder as the phone may put it in a message: a path relative to the chat's folder
/// (not empty, no leading `/`, no `..` part, no control character), whether it is a folder,
/// and a file's size or null.
fn suggested_file(entry: &Value) -> Option<Value> {
    let path = entry.get("path")?.as_str().filter(|path| {
        !path.is_empty()
            && !path.starts_with('/')
            && !path.split('/').any(|part| part == "..")
            && !has_control(path)
    })?;
    let bytes = match entry.get("bytes") {
        None | Some(Value::Null) => Value::Null,
        Some(bytes) => json!(bytes.as_u64()?),
    };
    Some(json!({"path": path, "dir": entry.get("dir")?.as_bool()?, "bytes": bytes}))
}

/// How long the CLI may take for a `chat.events` that waits up to `wait_ms`: the wait plus
/// the margin of any waiting call, and never less than any other call.
fn events_limit(wait_ms: i64) -> Duration {
    CLI_TIMEOUT
        .max(Duration::from_millis(wait_ms.clamp(0, EVENTS_WAIT_MS_MAX) as u64) + CLI_WAIT_MARGIN)
}

/// A validated `chat.command`: the chat and the command in the desktop's own JSON form,
/// rebuilt from the validated fields only.
#[derive(Debug, PartialEq)]
pub(super) struct CommandSpec {
    chat: String,
    command: Value,
}
pub(super) fn command_spec(params: &Value) -> std::result::Result<CommandSpec, Fault> {
    let object = fields(params, &["chat_id", "command"])?;
    let chat = chat_id(object)?;
    let command = object
        .get("command")
        .ok_or_else(|| invalid("command is required"))?;
    Ok(CommandSpec {
        chat,
        command: command_json(command)?,
    })
}
/// A provider request id: one short line.
fn request_id(object: &Map<String, Value>) -> std::result::Result<String, Fault> {
    let value = required(object, "request_id")?;
    if value.is_empty() || value.len() > REQUEST_ID_MAX_BYTES || has_control(value) {
        return Err(invalid(format!(
            "request_id must be 1..={REQUEST_ID_MAX_BYTES} bytes on one line"
        )));
    }
    Ok(value.to_owned())
}
/// The `ChatCommand` in `command`, strictly: its variant, its exact fields (no unknown field,
/// no null) and their limits. The result holds only what was validated.
fn command_json(command: &Value) -> std::result::Result<Value, Fault> {
    let kind = command
        .get("command")
        .ok_or_else(|| invalid("command.command is required"))?
        .as_str()
        .ok_or_else(|| invalid("command.command must be a string"))?;
    let allowed: &[&str] = match kind {
        "send" => &["command", "text"],
        "dismiss_notice" => &["command", "item_id"],
        "interrupt" | "compact" | "stop" => &["command"],
        "approve" => &["command", "request_id", "decision"],
        "answer" => &["command", "request_id", "answers"],
        "configure" => &[
            "command",
            "model",
            "effort",
            "approval_mode",
            "fast",
            "contributor_mode",
            "contributor_opt_out",
        ],
        "switch" => &["command", "provider", "model", "effort", "fast"],
        _ => {
            return Err(invalid(
                "command.command must be send, interrupt, approve, answer, configure, compact, stop, dismiss_notice or switch",
            ));
        }
    };
    let object = fields(command, allowed).map_err(|fault| {
        Fault::new(
            fault.code,
            fault.message.replace("params must", "command must"),
        )
    })?;
    Ok(match kind {
        "dismiss_notice" => {
            let item_id = required(object, "item_id")?;
            if item_id.is_empty() || item_id.len() > 512 || has_control(item_id) {
                return Err(invalid(
                    "item_id must be 1..=512 bytes without control characters",
                ));
            }
            json!({"command":"dismiss_notice","item_id":item_id})
        }
        "send" => {
            let text = required(object, "text")?;
            if text.trim().is_empty() || text.len() > TEXT_MAX {
                return Err(invalid(format!(
                    "text must be 1..={TEXT_MAX} bytes and not blank"
                )));
            }
            json!({"command":"send","text":text})
        }
        "approve" => {
            let decision = required(object, "decision")?;
            if !DECISIONS.contains(&decision) {
                return Err(invalid(
                    "decision must be accept, accept_for_session, decline or cancel",
                ));
            }
            json!({"command":"approve","request_id":request_id(object)?,"decision":decision})
        }
        "answer" => {
            json!({"command":"answer","request_id":request_id(object)?,"answers":answers(object)?})
        }
        "configure" => {
            let model = label("model", text(object, "model")?, MODEL_MAX_CHARS)?;
            let effort = label("effort", text(object, "effort")?, EFFORT_MAX_CHARS)?;
            let approval_mode = text(object, "approval_mode")?.map(mode).transpose()?;
            let fast = flag(object, "fast")?;
            let contributor_mode = contributor(object, true)?;
            let mut changes = Map::from_iter([("command".to_owned(), json!("configure"))]);
            for (name, value) in [
                ("model", model.map(Value::from)),
                ("effort", effort.map(Value::from)),
                ("approval_mode", approval_mode.map(Value::from)),
                ("fast", fast.map(Value::from)),
                ("contributor_mode", contributor_mode.map(Value::from)),
            ] {
                if let Some(value) = value {
                    changes.insert(name.to_owned(), value);
                }
            }
            if changes.len() == 1 {
                return Err(invalid(
                    "configure needs a model, an effort, an approval_mode, fast or contributor_mode",
                ));
            }
            Value::Object(changes)
        }
        "switch" => {
            let provider = match required(object, "provider")? {
                "codex" => "codex",
                "claude" => "claude",
                "muse" => "muse",
                _ => return Err(invalid("provider must be codex, claude or muse")),
            };
            let mut switch = Map::from_iter([
                ("command".to_owned(), json!("switch")),
                ("provider".to_owned(), json!(provider)),
            ]);
            for (name, value) in [
                (
                    "model",
                    label("model", text(object, "model")?, MODEL_MAX_CHARS)?.map(Value::from),
                ),
                (
                    "effort",
                    label("effort", text(object, "effort")?, EFFORT_MAX_CHARS)?.map(Value::from),
                ),
                ("fast", flag(object, "fast")?.map(Value::from)),
            ] {
                if let Some(value) = value {
                    switch.insert(name.to_owned(), value);
                }
            }
            Value::Object(switch)
        }
        other => json!({ "command": other }),
    })
}
/// `answers` of an `Answer`: one list of strings per question, in order.
fn answers(object: &Map<String, Value>) -> std::result::Result<Vec<Vec<String>>, Fault> {
    let questions = object
        .get("answers")
        .ok_or_else(|| invalid("answers is required"))?
        .as_array()
        .ok_or_else(|| invalid("answers must be an array of arrays of strings"))?;
    if questions.is_empty() || questions.len() > ANSWER_QUESTIONS_MAX {
        return Err(invalid(format!(
            "answers must hold 1..={ANSWER_QUESTIONS_MAX} questions"
        )));
    }
    let mut total = 0usize;
    let mut all = Vec::with_capacity(questions.len());
    for question in questions {
        let choices = question
            .as_array()
            .ok_or_else(|| invalid("answers must be an array of arrays of strings"))?;
        if choices.len() > ANSWER_CHOICES_MAX {
            return Err(invalid(format!(
                "a question takes at most {ANSWER_CHOICES_MAX} answers"
            )));
        }
        let mut given = Vec::with_capacity(choices.len());
        for choice in choices {
            let choice = choice
                .as_str()
                .ok_or_else(|| invalid("answers must be an array of arrays of strings"))?;
            if choice.len() > ANSWER_BYTES_MAX {
                return Err(invalid(format!(
                    "an answer may hold at most {ANSWER_BYTES_MAX} bytes"
                )));
            }
            total += choice.len();
            given.push(choice.to_owned());
        }
        all.push(given);
    }
    if total > ANSWER_TOTAL_MAX {
        return Err(invalid(format!(
            "answers may hold at most {ANSWER_TOTAL_MAX} bytes in all"
        )));
    }
    Ok(all)
}

/// A validated `chat.stop`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct StopSpec {
    chat: String,
}
pub(super) fn stop_spec(params: &Value) -> std::result::Result<StopSpec, Fault> {
    let object = fields(params, &["chat_id"])?;
    Ok(StopSpec {
        chat: chat_id(object)?,
    })
}

// ---- What the CLI says -------------------------------------------------------------------------

/// What a failed `riwork chat ...` says, by the first line of its error (`riwork: ` already
/// stripped). The wording belongs to the CLI and was there before these methods; the codes
/// are the connector's.
fn chat_fault(fault: Fault) -> Fault {
    if fault.code == "cli_error" && fault.message.contains("response_too_large:") {
        return Fault::new(
            "response_too_large",
            "This update is too large to show safely. Open this chat on your Mac.",
        );
    }
    if fault.code != "cli_error" {
        return fault;
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
    } else if line.starts_with("Unknown chat ") || line.starts_with("unknown chat ") {
        Fault::new("not_found", "chat not found on the desktop")
    } else if let Some(reason) = line.strip_prefix("invalid_request: ") {
        Fault::new("invalid_request", reason)
    } else if ["codex", "claude", "muse"].into_iter().any(installed)
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
/// What a failed `riwork chat command` says. The command may have been taken even so, when
/// the connector stopped waiting for the CLI.
fn command_fault(fault: Fault) -> Fault {
    if fault.code == "cli_error" && fault.message == "RiWork CLI timeout" {
        return Fault::new(
            "cli_error",
            "the chat took too long to answer and was left; read its events before trying again",
        );
    }
    chat_fault(fault)
}
/// What a failed `riwork chat new` says. The chat may exist even so, when the connector
/// stopped waiting for the CLI.
fn create_chat_fault(fault: Fault) -> Fault {
    if fault.code == "cli_error" && fault.message == "RiWork CLI timeout" {
        return Fault::new(
            "cli_error",
            "starting the chat took too long and was stopped; check the chat list before trying again",
        );
    }
    chat_fault(fault)
}
/// A chat that is `value`, as far as the phone needs to rely on it: an object with a
/// canonical id, a known provider, a title and directory, and a state.
fn chat_info(value: &Value) -> bool {
    let text = |name: &str| value.get(name).and_then(Value::as_str);
    text("id").is_some_and(|chat| id(chat).is_ok())
        && matches!(text("provider"), Some("codex" | "claude" | "muse"))
        && text("title").is_some()
        && text("cwd").is_some()
        && value.get("created_at_unix").is_some_and(Value::is_u64)
        && value
            .get("state")
            .is_some_and(|state| state.get("state").is_some_and(|name| name.is_string()))
}
/// The `chat.create` result for the chat the CLI printed, or `None` if it is not the chat
/// that was asked for: the provider, the project or worktree, the mode, the model, effort
/// and fast mode when they were sent, and a Muse chat's contributor mode, which is off unless
/// asked on (a Muse chat off says `contributor_opt_out` true).
fn new_result(spec: &NewSpec, cli: &Value) -> Option<Value> {
    let text = |name: &str| cli.get(name).and_then(Value::as_str);
    let in_target = match &spec.target {
        CreateTarget::Project(project) => text("project_id") == Some(project),
        CreateTarget::Worktree(worktree) => text("worktree_id") == Some(worktree),
    };
    let same = |name: &str, asked: &Option<String>| {
        asked
            .as_deref()
            .is_none_or(|asked| text(name) == Some(asked))
    };
    (chat_info(cli)
        && text("provider") == Some(spec.provider)
        && in_target
        && text("approval_mode") == Some(spec.mode)
        && same("model", &spec.model)
        && same("effort", &spec.effort)
        && (!spec.fast || cli.get("fast") == Some(&Value::Bool(true)))
        && (spec.provider != "muse"
            || if spec.contributor_on {
                text("contributor_mode") == Some("on")
            } else {
                cli.get("contributor_opt_out") == Some(&Value::Bool(true))
                    && cli.get("contributor_mode").is_none()
            }))
    .then(|| json!({ "chat": cli }))
}

/// A page of events as the CLI printed it, checked: `chat_id` is the chat, `events` are
/// `{seq, event}` in rising order after `since`, `next` is where the next call goes on.
/// Only those four fields (and `seq` and `event` of an entry) are passed on: a newer CLI
/// may print more, and the phone is told what the contract says.
#[derive(Debug, PartialEq)]
struct Page {
    chat: String,
    events: Vec<Value>,
    next: u64,
    more: bool,
}
impl Page {
    fn parse(spec: &EventsSpec, cli: &Value) -> Option<Self> {
        let object = cli.as_object()?;
        if object.get("chat_id").and_then(Value::as_str) != Some(spec.chat.as_str()) {
            return None;
        }
        let listed = object.get("events")?.as_array()?;
        if listed.len() as u64 > spec.max_events {
            return None;
        }
        let mut events = Vec::with_capacity(listed.len());
        let mut last = spec.since;
        for entry in listed {
            let entry = entry.as_object()?;
            let seq = entry.get("seq")?.as_u64()?;
            let event = entry.get("event")?;
            // An event is an object with its tag; what is inside is the desktop's.
            if seq <= last
                || ((spec.complete || spec.bounded) && last.checked_add(1) != Some(seq))
                || !event.get("event").is_some_and(Value::is_string)
            {
                return None;
            }
            last = seq;
            events.push(json!({"seq": seq, "event": event}));
        }
        let next = object.get("next")?.as_u64()?;
        let more = object.get("more")?.as_bool()?;
        // Not before the last event it returned, and a page that says there is more must have
        // moved the phone on, or it would ask for the same page again.
        if next < last
            || ((spec.complete || spec.bounded) && next != last)
            || (more && next == spec.since)
        {
            return None;
        }
        Some(Self {
            chat: spec.chat.clone(),
            events,
            next,
            more,
        })
    }
    /// Negotiate after validating the full CLI page. Legacy Swift checks contiguous
    /// seqs in complete/bounded mode: keep those slots using a supported no-op.
    /// An empty request id cannot identify an actionable host question.
    fn negotiate_rate_limits(&mut self, supported: bool, contiguous: bool) {
        if supported {
            return;
        }
        if contiguous {
            for entry in &mut self.events {
                if entry["event"]["event"] == "rate_limits" {
                    entry["event"] = json!({"event":"question_resolved", "request_id":""});
                }
            }
        } else {
            self.events
                .retain(|entry| entry["event"]["event"] != "rate_limits");
        }
    }

    /// The seq of the last event held, `None` for none.
    fn last_seq(&self) -> Option<u64> {
        self.events
            .last()
            .and_then(|entry| entry.get("seq"))
            .and_then(Value::as_u64)
    }
}
// Keep the connector's payload recovery local so its production binary remains
// independent of the engine crate. This is the same implementation as
// riwork_engine::chat::remote_payload; preserve its wire behavior when updating it.
mod remote_payload;
#[cfg(test)]
use remote_payload::cut_strings;
use remote_payload::{MIN_CUT, elide_event, shorten_body};
/// Reserve space for server_ms added at sealing time.
const FRAME_MARGIN: usize = 64;

fn fit_bounded_page(
    request: &str,
    mut page: Page,
    compress: bool,
) -> std::result::Result<Value, Fault> {
    loop {
        if let Some(result) = sealed(request, &page, compress)? {
            return Ok(result);
        }
        if page.events.len() > 1 {
            page.events.truncate(page.events.len() / 2);
            page.next = page.last_seq().unwrap();
            page.more = true;
            continue;
        }
        if let Some(entry) = page.events.first().cloned() {
            let mut cap = 64 * 1024;
            while cap >= MIN_CUT {
                let mut shrunk = entry.clone();
                if !shorten_body(&mut shrunk["event"], cap) {
                    break;
                }
                page.events[0] = shrunk;
                if let Some(result) = sealed(request, &page, compress)? {
                    return Ok(result);
                }
                cap /= 2;
            }
            page.events[0] = entry;
        }
        if let Some(entry) = page.events.first_mut() {
            entry["event"] = elide_event(&entry["event"]);
            if let Some(result) = sealed(request, &page, compress)? {
                return Ok(result);
            }
        }
        return Err(cli_fault("chat reply metadata exceeds frame limit"));
    }
}

/// The `chat.events` result for `page` if, with the rest of the reply to `request`, it fits
/// one sealed frame the way the connection will seal it (`compress`: deflated, as the session
/// opted in, or plain JSON); `None` if it does not.
fn sealed(request: &str, page: &Page, compress: bool) -> std::result::Result<Option<Value>, Fault> {
    let result = json!({
        "chat_id": page.chat,
        "events": page.events,
        "next": page.next,
        "more": page.more
    });
    let mut response = success(request, result);
    match link::encode_reply(
        &response,
        std::time::Instant::now(),
        compress,
        MAX_PLAINTEXT - FRAME_MARGIN,
    ) {
        Ok(_) => Ok(Some(response["result"].take())),
        Err(link::EncodeError::TooLarge) => Ok(None),
        Err(link::EncodeError::Other(e)) => Err(cli_fault(e)),
    }
}
/// All modes preserve sequence slots, recovering oversized single events identically.
fn fit_complete_page(
    request: &str,
    page: Page,
    compress: bool,
) -> std::result::Result<Value, Fault> {
    fit_bounded_page(request, page, compress)
}
fn fit_page(request: &str, page: Page, compress: bool) -> std::result::Result<Value, Fault> {
    fit_bounded_page(request, page, compress)
}

/// Keep snapshot pagination and row identity while recovering oversized payloads.
fn fit_snapshot(
    request: &str,
    result: Value,
    compress: bool,
    paginate: bool,
) -> std::result::Result<Value, Fault> {
    remote_payload::fit_snapshot(
        result,
        MAX_PLAINTEXT - REPLY_SLACK - FRAME_MARGIN,
        paginate,
        |result| match link::encode_reply(
            &success(request, result.clone()),
            std::time::Instant::now(),
            compress,
            MAX_PLAINTEXT - FRAME_MARGIN,
        ) {
            Ok(_) => Ok(true),
            Err(link::EncodeError::TooLarge) => Ok(false),
            Err(link::EncodeError::Other(e)) => Err(cli_fault(e)),
        },
    )?
    .ok_or_else(|| {
        Fault::new(
            "snapshot_limit",
            "snapshot metadata or requested rows exceed frame limit",
        )
    })
}

/// The `chat.suggestions` result for `request`, cut to fit one sealed frame the way the
/// connection will seal it (`compress`): the most of the first files and folders that fit, and
/// `truncated` set if any were left out. A reply that does not fit with none is a `cli_error`.
fn fit_suggestions(
    request: &str,
    mut result: Value,
    compress: bool,
) -> std::result::Result<Value, Fault> {
    let fits = |result: &Value| match link::encode_reply(
        &success(request, result.clone()),
        std::time::Instant::now(),
        compress,
        MAX_PLAINTEXT - FRAME_MARGIN,
    ) {
        Ok(_) => Ok(true),
        Err(link::EncodeError::TooLarge) => Ok(false),
        Err(link::EncodeError::Other(e)) => Err(cli_fault(e)),
    };
    if fits(&result)? {
        return Ok(result);
    }
    let mut entries = match result["files"]["entries"].take() {
        Value::Array(entries) => entries,
        _ => Vec::new(),
    };
    result["files"]["truncated"] = json!(true);
    // The most entries known to fit, and the fewest known not to.
    let (mut fit, mut over) = (0, entries.len());
    result["files"]["entries"] = json!([]);
    if !fits(&result)? {
        // Only a provider's very long model list is left: the first models that fit, so that
        // the commands still reach the phone.
        let models = match result["models"].take() {
            Value::Array(models) => models,
            _ => Vec::new(),
        };
        let (mut fit, mut over) = (0, models.len());
        result["models"] = json!([]);
        if !fits(&result)? {
            return Err(cli_fault("chat suggestions exceed frame limit"));
        }
        while over - fit > 1 {
            let middle = (fit + over) / 2;
            result["models"] = Value::Array(models[..middle].to_vec());
            if fits(&result)? {
                fit = middle;
            } else {
                over = middle;
            }
        }
        result["models"] = Value::Array(models[..fit].to_vec());
        return Ok(result);
    }
    while over - fit > 1 {
        let middle = (fit + over) / 2;
        result["files"]["entries"] = Value::Array(entries[..middle].to_vec());
        if fits(&result)? {
            fit = middle;
        } else {
            over = middle;
        }
    }
    entries.truncate(fit);
    result["files"]["entries"] = Value::Array(entries);
    Ok(result)
}

// ---- The methods -------------------------------------------------------------------------------

impl Rpc {
    /// Whether the installed CLI has the chat commands (`riwork capabilities --json` has
    /// `"chat":true`): `Ok(false)` if it ran and did not say so (an older CLI refuses the
    /// question), an error if it could not be run or took longer than `limit` in all. Only a
    /// yes is remembered: a CLI updated while the connector runs is believed at once. Askers
    /// at the same time run the CLI once: the others wait for the answer, within their own
    /// `limit`.
    async fn chat_known(&self, limit: Duration) -> std::result::Result<bool, Fault> {
        self.capability_known(&self.chat, limit).await
    }
    /// Whether `flag`, one of the booleans of this `Rpc` that remember a yes from
    /// `riwork capabilities --json`, is set, asking the CLI if it is not (see `chat_known`).
    /// One answer sets every flag it says yes to, so the questions of a handshake cost one
    /// run of the CLI between them.
    pub(super) async fn capability_known(
        &self,
        flag: &AtomicBool,
        limit: Duration,
    ) -> std::result::Result<bool, Fault> {
        if flag.load(Ordering::Relaxed) {
            return Ok(true);
        }
        let started = std::time::Instant::now();
        let answers = self.capability_answers.load(Ordering::Acquire);
        let Ok(_asking) = timeout(limit, self.asking_chat.lock()).await else {
            return Err(cli_fault("RiWork CLI timeout"));
        };
        // Answered while this one waited (a handshake asks about several at once): that
        // answer stands, a no as much as a yes.
        if flag.load(Ordering::Relaxed)
            || self.capability_answers.load(Ordering::Acquire) != answers
        {
            return Ok(flag.load(Ordering::Relaxed));
        }
        let asked = self
            .raw_within(
                vec!["capabilities".into(), "--json".into()],
                limit.saturating_sub(started.elapsed()),
            )
            .await;
        match asked {
            Ok(data) => {
                let reply = serde_json::from_slice::<Value>(&data).unwrap_or(Value::Null);
                if reply.get("v") == Some(&json!(1)) {
                    for (flag, name) in [
                        (&self.chat, "chat"),
                        (&self.tabs, "tabs"),
                        (&self.orchestrator_create, "orchestrator_create"),
                        (&self.shell_create_as_settings, "shell_create_as_settings"),
                        (&self.chat_provider_switch, "chat_provider_switch"),
                        (&self.chat_models, "chat_models"),
                        (&self.chat_suggestions, "chat_suggestions"),
                        (&self.dashboard, "dashboard"),
                        (&self.project_memo, "project_memo"),
                    ] {
                        if reply.get(name) == Some(&Value::Bool(true)) {
                            flag.store(true, Ordering::Relaxed);
                        }
                    }
                }
                self.capability_answers.fetch_add(1, Ordering::Release);
                Ok(flag.load(Ordering::Relaxed))
            }
            // It ran and refused the question: a CLI from before `capabilities`.
            Err(e) if e.to_string().starts_with("RiWork CLI failed") => {
                self.capability_answers.fetch_add(1, Ordering::Release);
                Ok(false)
            }
            Err(e) => Err(cli_fault(e)),
        }
    }
    /// What `ready` announces as `features.chat`. The question is short: a CLI that does not
    /// answer within a few seconds does not delay the handshake any longer (the phone gives
    /// the whole handshake ten).
    pub async fn chat_supported(&self) -> bool {
        self.chat_known(Duration::from_secs(3))
            .await
            .unwrap_or(false)
    }
    /// What `ready` announces as `features.chat_provider_switch`: a chat takes the `switch`
    /// command. Short, like `chat_supported`, whose question it shares.
    pub async fn chat_provider_switch_supported(&self) -> bool {
        self.capability_known(&self.chat_provider_switch, Duration::from_secs(3))
            .await
            .unwrap_or(false)
    }
    /// What `ready` announces as `features.chat_models`: `chat.models` lists a provider's
    /// saved models.
    pub async fn chat_models_supported(&self) -> bool {
        self.capability_known(&self.chat_models, Duration::from_secs(3))
            .await
            .unwrap_or(false)
    }
    /// What `ready` announces as `features.chat_suggestions`: `chat.suggestions` lists a
    /// chat's commands and files.
    pub async fn chat_suggestions_supported(&self) -> bool {
        self.capability_known(&self.chat_suggestions, Duration::from_secs(3))
            .await
            .unwrap_or(false)
    }
    /// Ask the CLI about chats ahead of time, with all the time a CLI call gets, so that
    /// `chat_supported` finds the answer when a phone arrives. The first run of a CLI that was
    /// just updated can take seconds.
    pub async fn learn_chat(&self) {
        let _ = self.chat_known(CLI_TIMEOUT).await;
    }
    async fn require_chat(&self) -> std::result::Result<(), Fault> {
        if self.chat_known(CLI_TIMEOUT).await? {
            Ok(())
        } else {
            Err(Fault::new(
                "cli_error",
                "the installed riwork CLI does not support chats; update RiWork",
            ))
        }
    }
    /// The device may still act: it was authorized when the request started, and a request
    /// that waited in the ordered lane may have been revoked since.
    pub(super) fn still_authorized(&self, device: &str) -> std::result::Result<(), Fault> {
        if self.storage.authorized(device).map_err(cli_fault)? {
            Ok(())
        } else {
            Err(Fault::new("not_found", "device revoked"))
        }
    }

    /// Every chat of the desktop, or the chats of one project, which must exist under exactly
    /// the id given.
    pub(super) async fn chats_list(
        &self,
        spec: ListSpec,
        reply_limit: usize,
    ) -> std::result::Result<Value, Fault> {
        self.require_chat().await?;
        let mut args: Vec<String> = vec!["chat".into(), "list".into()];
        if let Some(project) = &spec.project {
            self.target_exists(&CreateTarget::Project(project.clone()), chat_fault)
                .await?;
            args.extend(["--project".into(), project.clone()]);
        }
        // As big as a reply may be for this session; a list past that is too long to send.
        let listed = array(
            self.read_capped(args, CLI_TIMEOUT, reply_limit)
                .await
                .map_err(|fault| {
                    if fault.code == "response_too_large" {
                        Fault::new(
                            "response_too_large",
                            "there are too many chats for one encrypted response; list one project's chats",
                        )
                    } else {
                        chat_fault(fault)
                    }
                })?,
        )?;
        let mut chats = Vec::with_capacity(listed.len());
        for chat in listed {
            let in_project = spec.project.as_deref().is_none_or(|project| {
                chat.get("project_id").and_then(Value::as_str) == Some(project)
            });
            if !chat_info(&chat) || !in_project {
                return Err(cli_fault(
                    "CLI returned a chat that does not fit the request",
                ));
            }
            chats.push(chat);
        }
        Ok(json!({ "chats": chats }))
    }

    /// The models a chat of a provider can be set to, as `riwork chat models` reads them
    /// from saved chats: for a chat that does not run that provider yet. Starts no host and
    /// no agent.
    pub(super) async fn chat_models(
        &self,
        spec: ModelsSpec,
        reply_limit: usize,
    ) -> std::result::Result<Value, Fault> {
        self.require_chat().await?;
        if !self
            .capability_known(&self.chat_models, CLI_TIMEOUT)
            .await?
        {
            return Err(Fault::new(
                "cli_error",
                "the installed riwork CLI cannot list saved chat models; update RiWork",
            ));
        }
        let mut args: Vec<String> = vec![
            "chat".into(),
            "models".into(),
            "--provider".into(),
            spec.provider.into(),
        ];
        if let Some(project) = &spec.project {
            self.target_exists(&CreateTarget::Project(project.clone()), chat_fault)
                .await?;
            args.extend(["--project".into(), project.clone()]);
        }
        let listed = self
            .read_capped(args, CLI_TIMEOUT, reply_limit)
            .await
            .map_err(chat_fault)?;
        models_result(spec.provider, &listed)
            .ok_or_else(|| cli_fault("CLI returned models that do not fit the request"))
    }

    /// What a chat's message box offers for `/` and `@`, as `riwork chat suggestions` reads it:
    /// the chat's commands and the files and folders of its folder, from one bounded walk. Starts
    /// no host and no agent. `request` is the id of the request, for sizing the reply;
    /// `reply_limit` tells whether the session deflates its replies.
    pub(super) async fn chat_suggestions(
        &self,
        request: &str,
        spec: SuggestionsSpec,
        reply_limit: usize,
    ) -> std::result::Result<Value, Fault> {
        self.require_chat().await?;
        if !self
            .capability_known(&self.chat_suggestions, CLI_TIMEOUT)
            .await?
        {
            return Err(Fault::new(
                "cli_error",
                "the installed riwork CLI cannot list chat suggestions; update RiWork",
            ));
        }
        // A big folder's listing is bigger than a frame; `fit_suggestions` cuts what is sent.
        // The CLI prints at most 4 MiB (`riwork_engine::chat::suggest::REPLY_BYTES`), cut by
        // itself, so the read allowance is never what refuses a listing.
        let listed = self
            .read_capped(
                vec!["chat".into(), "suggestions".into(), spec.chat.clone()],
                CLI_TIMEOUT,
                CHAT_READ_MAX,
            )
            .await
            .map_err(chat_fault)?;
        let result = suggestions_result(&spec.chat, &listed)
            .ok_or_else(|| cli_fault("CLI returned suggestions that do not fit the request"))?;
        let request = request.to_owned();
        tokio::task::spawn_blocking(move || {
            fit_suggestions(&request, result, reply_limit > MAX_PLAINTEXT)
        })
        .await
        .map_err(|e| cli_fault(format!("fitting the suggestions was interrupted: {e}")))?
    }

    /// Start a chat in a project or worktree that exists on the desktop, as `riwork chat
    /// new` does: the chat host records the chat and starts its agent. Runs in the ordered
    /// lane, which a phone that drops does not cut short, and the CLI itself runs in a task of
    /// its own (see below). Not idempotent: a repeat starts another chat. A chat whose agent
    /// cannot start is still created: it is the answer, with the failed state in it.
    pub(super) async fn chat_create(
        &self,
        device: &str,
        spec: NewSpec,
    ) -> std::result::Result<Value, Fault> {
        self.require_chat().await?;
        self.target_exists(&spec.target, chat_fault).await?;
        // Authorization was checked when the request started; this acts.
        self.still_authorized(device)?;
        // The host writes the chat down and then starts its agent; a CLI killed in between
        // leaves a chat nobody was told about. The connection's tasks are dropped (and their
        // CLI processes killed) when it ends for any reason, so the CLI runs in a task that
        // outlives the request: if the request is dropped, only the answer is lost.
        let runner = self.detached();
        let args = new_args(&spec);
        let created = tokio::spawn(async move { runner.read_within(args, CREATE_TIMEOUT).await })
            .await
            .map_err(|e| cli_fault(format!("creating the chat was interrupted: {e}")))?
            .map_err(create_chat_fault)?;
        new_result(&spec, &created).ok_or_else(|| {
            // Not what was asked for. It was just made, so stop its agent rather than leave
            // a chat the phone knows nothing about running.
            if let Some(stray) = created.get("id").and_then(Value::as_str)
                && id(stray).is_ok()
            {
                let (runner, stray) = (self.detached(), stray.to_owned());
                tokio::spawn(async move {
                    let _ = runner.raw(vec!["chat".into(), "stop".into(), stray]).await;
                });
            }
            cli_fault("CLI returned a chat that does not match the request")
        })
    }

    pub(super) async fn chat_snapshot(
        &self,
        request: &str,
        spec: SnapshotSpec,
        reply_limit: usize,
    ) -> std::result::Result<Value, Fault> {
        let budget = (MAX_PLAINTEXT - REPLY_SLACK - FRAME_MARGIN).min(reply_limit - REPLY_SLACK);
        let mut args = vec![
            "chat".into(),
            "snapshot".into(),
            spec.chat.clone(),
            "--max".into(),
            spec.limit.to_string(),
            "--max-bytes".into(),
            budget.to_string(),
        ];
        if spec.rate_limits {
            args.push("--rate-limits".into());
        }
        if let Some(cursor) = &spec.cursor {
            args.extend([
                "--cursor".into(),
                cursor.clone(),
                "--before".into(),
                spec.before.to_string(),
            ]);
        }
        if !spec.items.is_empty() {
            args.push(format!(
                "--items-json={}",
                serde_json::to_string(&spec.items).map_err(cli_fault)?
            ));
        }
        let mut result = self
            .read_capped(args, CLI_TIMEOUT, CHAT_READ_MAX)
            .await
            .map_err(|fault| {
                if fault.message.contains("Unknown chat command")
                    || fault.message.contains("Usage: riwork chat")
                {
                    Fault::new(
                        "invalid_request",
                        "unsupported RPC method: update desktop CLI for chat.snapshot",
                    )
                } else if fault.message.contains("snapshot")
                    && fault.message.contains("limit exceeded")
                {
                    Fault::new(
                        "snapshot_limit",
                        "This chat is too long to load here. Open it on your Mac.",
                    )
                } else if fault.message.contains("chat not found") {
                    Fault::new("not_found", "chat not found on the desktop")
                } else if fault.message.contains("snapshot expired") {
                    Fault::new(
                        "snapshot_expired",
                        "snapshot expired; reopen the recent window",
                    )
                } else {
                    chat_fault(fault)
                }
            })?;
        let valid = result["v"] == json!(1)
            && result["chat_id"] == spec.chat
            && result["next"].as_u64().is_some()
            && result["cursor"].as_str().is_some_and(|c| c.len() <= 80)
            && result["more"].is_boolean()
            && result["before"].as_u64().is_some()
            && result["controls"].as_array().is_some()
            && result["items"].as_array().is_some_and(|items| {
                let mut last = 0;
                items.len()
                    <= if spec.items.is_empty() {
                        spec.limit as usize
                    } else {
                        spec.items.len()
                    }
                    && items.iter().all(|i| {
                        let order = i["order"].as_u64().unwrap_or(0);
                        let good =
                            order > last && order < spec.before && i["item"]["id"].is_string();
                        last = order;
                        good
                    })
            })
            && spec
                .cursor
                .as_ref()
                .is_none_or(|c| result["cursor"] == *c && result["controls"] == json!([]));
        if !valid {
            return Err(cli_fault("invalid snapshot reply"));
        }
        // Also guard replies from an older CLI that emitted quota controls by default.
        negotiate_snapshot_rate_limits(&mut result, spec.rate_limits);
        let request = request.to_owned();
        tokio::task::spawn_blocking(move || {
            fit_snapshot(
                &request,
                result,
                reply_limit > MAX_PLAINTEXT,
                spec.items.is_empty(),
            )
        })
        .await
        .map_err(|e| cli_fault(format!("fitting the snapshot was interrupted: {e}")))?
    }

    /// A page of a chat's events after `since`, as `riwork chat events` collects it. `request`
    /// is the id of the request, for sizing the reply; `reply_limit` tells whether the session
    /// deflates its replies.
    pub(super) async fn chat_events(
        &self,
        request: &str,
        spec: EventsSpec,
        reply_limit: usize,
    ) -> std::result::Result<Value, Fault> {
        self.require_chat().await?;
        // Bigger than one frame is fine when the session deflates; `fit_page` makes sure
        // that what is sent fits the frame it ends up in.
        let mut args: Vec<String> = [
            "chat".to_owned(),
            "events".into(),
            spec.chat.clone(),
            "--since".into(),
            spec.since.to_string(),
            "--wait-ms".into(),
            spec.wait_ms.to_string(),
            "--max".into(),
            spec.max_events.to_string(),
            "--max-bytes".into(),
            (reply_limit - REPLY_SLACK).to_string(),
        ]
        .into();
        if spec.complete {
            args.push("--complete".into());
        }
        if spec.bounded {
            args.push("--bounded".into());
        }
        let cli = self
            .read_capped(args, events_limit(spec.wait_ms), CHAT_READ_MAX)
            .await
            .map_err(|fault| {
                if fault.code == "response_too_large" {
                    Fault::new(
                        "response_too_large",
                        "the page exceeds the encrypted response limit; request fewer events",
                    )
                } else {
                    chat_fault(fault)
                }
            })?;
        let mut page = Page::parse(&spec, &cli).ok_or_else(|| {
            Fault::new(
                "invalid_reply",
                "CLI returned a page of events that does not fit the request",
            )
        })?;
        page.negotiate_rate_limits(spec.rate_limits, spec.complete || spec.bounded);
        // Deflating a page of megabytes, more than once, is not for a thread that serves others.
        let (request, compress) = (request.to_owned(), reply_limit > MAX_PLAINTEXT);
        tokio::task::spawn_blocking(move || {
            if spec.complete {
                fit_complete_page(&request, page, compress)
            } else if spec.bounded {
                fit_bounded_page(&request, page, compress)
            } else {
                fit_page(&request, page, compress)
            }
        })
        .await
        .map_err(|e| cli_fault(format!("fitting the page was interrupted: {e}")))?
    }

    /// Hand one command to a chat. A message to a stopped chat resumes it, which starts its
    /// agent: the same allowance as starting a chat. Ordered, like typing.
    pub(super) async fn chat_command(
        &self,
        device: &str,
        spec: CommandSpec,
    ) -> std::result::Result<Value, Fault> {
        self.require_chat().await?;
        // An older CLI would refuse it as a command it does not know; this says why.
        if spec.command.get("command") == Some(&json!("switch"))
            && !self
                .capability_known(&self.chat_provider_switch, CLI_TIMEOUT)
                .await?
        {
            return Err(Fault::new(
                "cli_error",
                "the installed riwork CLI cannot move a chat to another provider; update RiWork",
            ));
        }
        self.still_authorized(device)?;
        let command = serde_json::to_string(&spec.command).map_err(cli_fault)?;
        let args = vec![
            "chat".to_owned(),
            "command".into(),
            spec.chat.clone(),
            "--command-json".into(),
            command,
        ];
        let done = self
            .read_within(args, COMMAND_TIMEOUT)
            .await
            .map_err(command_fault)?;
        if done.get("id").and_then(Value::as_str) != Some(spec.chat.as_str()) {
            return Err(cli_fault("CLI answered for another chat"));
        }
        Ok(json!({"status":"ok"}))
    }

    /// Stop a chat's agent and keep its history. With no chat host running nothing runs, so
    /// every chat is stopped already.
    pub(super) async fn chat_stop(
        &self,
        device: &str,
        spec: StopSpec,
    ) -> std::result::Result<Value, Fault> {
        self.require_chat().await?;
        self.still_authorized(device)?;
        match self.read(&["chat", "stop", &spec.chat]).await {
            Ok(done) if done.get("id").and_then(Value::as_str) == Some(spec.chat.as_str()) => {}
            Ok(_) => return Err(cli_fault("CLI answered for another chat")),
            Err(fault) => {
                let no_host = fault
                    .message
                    .strip_prefix("RiWork CLI failed: riwork: ")
                    .is_some_and(|detail| detail.starts_with("No chat host is running"));
                if !no_host {
                    return Err(command_fault(fault));
                }
            }
        }
        Ok(json!({"status":"stopped"}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limits_require_opt_in_and_filtered_pages_still_advance() {
        let id = "11111111-2222-4333-8444-555555555555";
        for mode in ["complete", "bounded"] {
            let mut request = json!({"chat_id":id,"since":0,"wait_ms":0,mode:true});
            let legacy = events_spec(&request).unwrap();
            assert!(!legacy.rate_limits);
            let raw = json!({"chat_id":id,"events":[{"seq":1,"event":{"event":"rate_limits","windows":[]}}],"next":1,"more":true});
            let mut page = Page::parse(&legacy, &raw).unwrap();
            page.negotiate_rate_limits(false, true);
            assert_eq!(page.events.len(), 1);
            assert_eq!(
                page.events[0],
                json!({"seq":1,"event":{"event":"question_resolved","request_id":""}})
            );
            assert_eq!(page.next, 1);
            assert!(page.more);
            request["features"] = json!(["rate_limits"]);
            let opted = events_spec(&request).unwrap();
            assert!(opted.rate_limits);
            let mut page = Page::parse(&opted, &raw).unwrap();
            page.negotiate_rate_limits(true, true);
            assert_eq!(page.events.len(), 1);
        }
        let raw_page = json!({"chat_id":id,"events":[{"seq":1,"event":{"event":"rate_limits","windows":[]}},event(2,"hello")],"next":2,"more":false});
        let legacy = events_spec(&json!({"chat_id":id,"since":0,"wait_ms":0})).unwrap();
        let mut page = Page::parse(&legacy, &raw_page).unwrap();
        page.negotiate_rate_limits(false, false);
        assert_eq!(page.events, vec![event(2, "hello")]);
        assert_eq!(page.next, 2);
        let raw = json!({"controls":[{"event":"state","state":{"state":"idle"}},{"event":"rate_limits","windows":[{"id":"primary"}]}]});
        let mut old_phone = raw.clone();
        negotiate_snapshot_rate_limits(&mut old_phone, false);
        assert_eq!(old_phone["controls"].as_array().unwrap().len(), 1);
        let mut opted = raw;
        negotiate_snapshot_rate_limits(&mut opted, true);
        assert_eq!(opted["controls"].as_array().unwrap().len(), 2);
        opted["controls"][1]["windows"] = json!([]);
        negotiate_snapshot_rate_limits(&mut opted, true);
        assert_eq!(opted["controls"].as_array().unwrap().len(), 1);
        assert!(!snapshot_spec(&json!({"chat_id":id})).unwrap().rate_limits);
        assert!(
            snapshot_spec(&json!({"chat_id":id,"features":["rate_limits"]}))
                .unwrap()
                .rate_limits
        );
        assert!(snapshot_spec(&json!({"chat_id":id,"features":"rate_limits"})).is_err());
    }

    fn event(seq: u64, text: &str) -> Value {
        json!({"seq": seq, "event": {"event": "item_completed", "item": {
            "id": format!("agent-{seq}"), "status": "completed",
            "body": {"type": "agent_message", "text": text}}}})
    }
    fn page(events: Vec<Value>) -> Page {
        Page {
            chat: "11111111-2222-4333-8444-555555555555".into(),
            next: events.len() as u64,
            events,
            more: false,
        }
    }

    #[test]
    fn frame_cut_does_not_corrupt_image_payloads() {
        let mut value =
            json!({"source":{"kind":"data","mime":"image/png","base64":"a".repeat(2000)}});
        cut_strings(&mut value, 128);
        assert_eq!(value["source"]["kind"], "unavailable");
        assert!(value["source"].get("base64").is_none());
    }

    #[test]
    fn a_wait_for_events_gets_the_wait_plus_a_margin_and_never_less_than_other_calls() {
        assert_eq!(events_limit(0), Duration::from_secs(15));
        assert_eq!(events_limit(7_000), Duration::from_secs(15));
        assert_eq!(events_limit(8_000), Duration::from_secs(16));
        assert_eq!(events_limit(EVENTS_WAIT_MS_MAX), Duration::from_secs(33));
        // Out of range never widens the limit.
        assert_eq!(events_limit(i64::MAX), events_limit(EVENTS_WAIT_MS_MAX));
        assert_eq!(events_limit(-1), Duration::from_secs(15));
    }

    #[test]
    fn a_page_that_fits_is_left_alone_and_a_cut_one_keeps_its_first_events() {
        let request = "99999999-2222-4333-8444-555555555555";
        // Nothing and a little fit whatever the session does with them.
        for compress in [false, true] {
            let quiet = fit_page(request, page(Vec::new()), compress).unwrap();
            assert_eq!(quiet["events"], json!([]));
            assert_eq!(quiet["more"], false);
        }
        // 40 events of 8 KiB of noise are 330 KB of JSON that deflate by little: a frame holds
        // about a third of them, in either form, and the cut is a prefix.
        let noise = |n: u64, length: usize| -> String {
            let mut x = n.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            (0..length)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    char::from(b'!' + (x % 90) as u8)
                })
                .filter(|c| *c != '"' && *c != '\\')
                .collect()
        };
        let events: Vec<Value> = (1..=40).map(|n| event(n, &noise(n, 8192))).collect();
        for compress in [false, true] {
            let cut = fit_page(request, page(events.clone()), compress).unwrap();
            let held = cut["events"].as_array().unwrap();
            assert!(!held.is_empty() && held.len() < 40, "{}", held.len());
            assert_eq!(held[..], events[..held.len()]);
            assert_eq!(cut["more"], true);
            assert_eq!(cut["next"], held.len() as u64);
        }
        // One event that is more than a frame by itself is cut until it fits, in either form:
        // the phone goes on past it, and what it gets is the start of what was there.
        for compress in [false, true] {
            let huge = event(1, &noise(1, 400_000));
            let cut = fit_page(request, page(vec![huge.clone()]), compress).unwrap();
            let held = cut["events"].as_array().unwrap();
            assert_eq!(held.len(), 1);
            assert_eq!(held[0]["seq"], 1);
            let text = held[0]["event"]["item"]["body"]["text"].as_str().unwrap();
            let original = huge["event"]["item"]["body"]["text"].as_str().unwrap();
            assert!(text.ends_with('\u{2026}') && text.len() < original.len());
            assert!(original.starts_with(text.trim_end_matches('\u{2026}')));
            assert_eq!(
                (cut["next"].as_u64(), cut["more"].clone()),
                (Some(1), json!(false))
            );
        }
        // One that cannot be cut small enough keeps its sequence slot as a control placeholder.
        let wide = json!({"seq": 1, "event": {"event": "x",
            "numbers": (0..60_000).map(|n| n * 7919).collect::<Vec<u64>>()}});
        let passed = fit_page(request, page(vec![wide.clone()]), false).unwrap();
        assert_eq!(
            passed["events"][0]["event"],
            json!({"event":"control_elided","of":"x","reason":"too_large","bytes":wide["event"].to_string().len(),"elided":true})
        );
        assert_eq!(passed["next"], 1);
    }

    #[test]
    fn a_label_is_bounded_in_characters_and_blank_is_none() {
        assert_eq!(
            label("title", Some("é".repeat(200).as_str()), 200)
                .unwrap()
                .unwrap()
                .len(),
            400
        );
        assert!(label("title", Some("é".repeat(201).as_str()), 200).is_err());
        assert_eq!(label("title", Some("   "), 200).unwrap(), None);
        assert_eq!(label("title", None, 200).unwrap(), None);
        assert!(label("title", Some("a\u{85}b"), 200).is_err());
    }

    #[test]
    fn notice_dismissal_command_is_validated_and_forwarded() {
        let command = json!({"command":"dismiss_notice","item_id":"notice-one"});
        assert_eq!(command_json(&command).unwrap(), command);
        for bad in [
            json!({"command":"dismiss_notice"}),
            json!({"command":"dismiss_notice","item_id":""}),
            json!({"command":"dismiss_notice","item_id":null}),
            json!({"command":"dismiss_notice","item_id":"bad\nline"}),
            json!({"command":"dismiss_notice","item_id":"x".repeat(513)}),
            json!({"command":"dismiss_notice","item_id":"one","kind":"auth_required"}),
        ] {
            assert!(command_json(&bad).is_err(), "{bad}");
        }
    }

    const PROJECT: &str = "11111111-2222-4333-8444-555555555555";

    #[test]
    fn fast_is_a_boolean_that_becomes_the_cli_flag_only_when_on() {
        let spec = |extra: Value| {
            let mut params = json!({"provider": "codex", "project_id": PROJECT});
            params
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            new_spec(&params)
        };
        let args = |spec: &NewSpec| new_args(spec);
        assert!(!spec(json!({})).unwrap().fast);
        assert!(!spec(json!({"fast": false})).unwrap().fast);
        let fast = spec(json!({"fast": true, "model": "gpt-5.5"})).unwrap();
        assert!(fast.fast);
        let argv = args(&fast);
        assert_eq!(argv.last().map(String::as_str), Some("--fast"));
        assert!(argv.contains(&"--model=gpt-5.5".to_owned()));
        assert!(!args(&spec(json!({"fast": false})).unwrap()).contains(&"--fast".to_owned()));
        // Nothing but a boolean: null, numbers, strings.
        for bad in [json!(null), json!(1), json!("true"), json!([true])] {
            let fault = spec(json!({"fast": bad})).unwrap_err();
            assert_eq!(fault.code, "invalid_request");
            assert!(
                fault.message.contains("fast must be a boolean"),
                "{}",
                fault.message
            );
        }
    }

    #[test]
    fn a_muse_chat_is_created_off_unless_asked_on_and_must_come_back_so() {
        let spec = |provider: &str, extra: Value| {
            let mut params = json!({"provider": provider, "project_id": PROJECT});
            params
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            new_spec(&params)
        };
        let on = "--contributor-on".to_owned();
        // Off, however a phone says it or leaves it out, and an older phone's false is no on.
        for extra in [
            json!({}),
            json!({"contributor_mode": "off"}),
            json!({"contributor_opt_out": true}),
            json!({"contributor_opt_out": false}),
            json!({"contributor_mode": "off", "contributor_opt_out": true}),
        ] {
            let off = spec("muse", extra.clone()).unwrap();
            assert!(!off.contributor_on, "{extra}");
            let args = new_args(&off);
            assert!(
                !args.iter().any(|arg| arg.starts_with("--contributor")),
                "{extra}: {args:?}"
            );
        }
        let asked_on = spec("muse", json!({"contributor_mode": "on"})).unwrap();
        assert!(asked_on.contributor_on);
        assert!(new_args(&asked_on).contains(&on));
        for bad in [
            json!({"contributor_mode": "ON"}),
            json!({"contributor_mode": "settings"}),
            json!({"contributor_mode": true}),
            json!({"contributor_mode": null}),
            json!({"contributor_opt_out": null}),
            json!({"contributor_opt_out": "off"}),
            json!({"contributor_mode": "on", "contributor_opt_out": true}),
        ] {
            let fault = spec("muse", bad.clone()).unwrap_err();
            assert_eq!(fault.code, "invalid_request", "{bad}");
        }
        // Only Muse can be asked on; off is what every other chat has anyway.
        for provider in ["codex", "claude"] {
            let fault = spec(provider, json!({"contributor_mode": "on"})).unwrap_err();
            assert!(fault.message.contains("only Muse"), "{}", fault.message);
            assert!(spec(provider, json!({"contributor_mode": "off"})).is_ok());
        }
        // The chat printed must be the one asked for: off as this host settles a Muse chat, or
        // on as itself. One printed with neither is from a host before it, which follows the
        // Muse settings.
        let printed = |provider: &str, extra: Value| {
            let mut chat = json!({
                "id": "11111111-2222-4333-8444-666666666666", "provider": provider,
                "project_id": PROJECT, "approval_mode": "supervised", "title": "Muse chat",
                "cwd": "/w", "created_at_unix": 1, "state": {"state": "idle"},
            });
            chat.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            chat
        };
        let off = spec("muse", json!({})).unwrap();
        let settled_off = printed("muse", json!({"contributor_opt_out": true}));
        let printed_on = printed("muse", json!({"contributor_mode": "on"}));
        assert!(new_result(&off, &settled_off).is_some());
        assert!(new_result(&off, &printed("muse", json!({}))).is_none());
        assert!(new_result(&off, &printed_on).is_none());
        assert!(new_result(&asked_on, &printed_on).is_some());
        assert!(new_result(&asked_on, &settled_off).is_none());
        assert!(new_result(&asked_on, &printed("muse", json!({}))).is_none());
        let codex = spec("codex", json!({})).unwrap();
        assert!(new_result(&codex, &printed("codex", json!({}))).is_some());
    }

    #[test]
    fn configure_takes_fast_alone_or_with_the_rest_and_nothing_else_counts_as_a_change() {
        let configure = |fields: Value| {
            let mut command = json!({"command": "configure"});
            command
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            command_json(&command)
        };
        assert_eq!(
            configure(json!({"fast": true})).unwrap(),
            json!({"command": "configure", "fast": true})
        );
        assert_eq!(
            configure(json!({"fast": false, "effort": "low"})).unwrap(),
            json!({"command": "configure", "effort": "low", "fast": false})
        );
        // No change at all, and a blank model that is left out, are still nothing.
        assert!(configure(json!({})).is_err());
        assert!(configure(json!({"model": "  "})).is_err());
        for bad in [json!(null), json!(0), json!("on")] {
            assert!(configure(json!({"fast": bad})).is_err(), "{bad}");
        }
        // Only a configure has it.
        let fault =
            command_json(&json!({"command": "send", "text": "hi", "fast": true})).unwrap_err();
        assert!(
            fault.message.contains("unknown field fast"),
            "{}",
            fault.message
        );
    }

    #[test]
    fn paired_contributor_commands_keep_the_exact_allowlist_and_refuse_privileged_neighbors() {
        let chat = "44444444-4444-4444-8444-444444444444";
        // Already allowed on the paired-device lane: iOS needs no broader Configure or
        // provider settings permission. The CLI/engine still enforce Muse and idle state.
        for mode in ["on", "off"] {
            let command = json!({"command": "configure", "contributor_mode": mode});
            let spec = command_spec(&json!({"chat_id": chat, "command": command})).unwrap();
            assert_eq!(spec.chat, chat);
            assert_eq!(spec.command, command);
        }
        for extra in [
            json!({"claude_billing": "api_key"}),
            json!({"claude_billing": "subscription"}),
            json!({"trust_workspace": true}),
            json!({"settings": {"contributor_mode": "on"}}),
            json!({"contributor_opt_out": false}),
        ] {
            let mut command = json!({"command": "configure", "contributor_mode": "on"});
            command
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let fault = command_spec(&json!({"chat_id": chat, "command": command})).unwrap_err();
            assert_eq!(fault.code, "invalid_request", "{extra}");
        }
    }

    #[test]
    fn configure_takes_contributor_mode_on_or_off_and_an_older_phones_off_but_never_its_false() {
        for mode in ["on", "off"] {
            assert_eq!(
                command_json(&json!({"command": "configure", "contributor_mode": mode})).unwrap(),
                json!({"command": "configure", "contributor_mode": mode})
            );
        }
        // An older phone's true is an off, said the new way; the CLI says it the older way too.
        for command in [
            json!({"command": "configure", "contributor_opt_out": true}),
            json!({"command": "configure", "contributor_mode": "off", "contributor_opt_out": true}),
        ] {
            assert_eq!(
                command_json(&command).unwrap(),
                json!({"command": "configure", "contributor_mode": "off"}),
                "{command}"
            );
        }
        for bad in [
            json!({"contributor_mode": "ON"}),
            json!({"contributor_mode": true}),
            json!({"contributor_mode": null}),
            json!({"contributor_opt_out": null}),
            json!({"contributor_opt_out": 1}),
            json!({"contributor_opt_out": "off"}),
            // The Muse settings, which a chat no longer follows, and on beside an off.
            json!({"contributor_opt_out": false}),
            json!({"contributor_mode": "off", "contributor_opt_out": false}),
            json!({"contributor_mode": "on", "contributor_opt_out": true}),
        ] {
            let mut command = json!({"command": "configure"});
            command
                .as_object_mut()
                .unwrap()
                .extend(bad.as_object().unwrap().clone());
            let fault = command_json(&command).unwrap_err();
            assert_eq!(fault.code, "invalid_request", "{bad}");
        }
        for command in [
            json!({"command": "send", "text": "hi", "contributor_mode": "on"}),
            json!({"command": "switch", "provider": "muse", "contributor_mode": "on"}),
            json!({"command": "send", "text": "hi", "contributor_opt_out": true}),
            json!({"command": "switch", "provider": "muse", "contributor_opt_out": true}),
        ] {
            let fault = command_json(&command).unwrap_err();
            assert!(
                fault.message.contains("unknown field contributor_"),
                "{}",
                fault.message
            );
        }
    }
}
