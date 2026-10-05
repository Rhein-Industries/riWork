//! Chats on the phone ("Chat extension" in `docs/remote-protocol.md`): `chats.list`,
//! `chat.create`, `chat.events`, `chat.command` and `chat.stop`, and `chat.options`, the
//! models and efforts a chat is offered.
//!
//! Each is one call of the installed CLI (`riwork chat ...`), which talks to the chat host.
//! The connector validates the params before any CLI runs, builds the argument vector from
//! the validated values (one argument per value, never a shell string), and checks what the
//! CLI answers before it reaches the phone. The chat JSON itself (`ChatInfo`, `ChatEvent`,
//! `ChatCommand`) is the desktop's `src/chat/model.rs` and is passed on as the CLI printed
//! it: the phone decodes it leniently.
//!
//! - `chat.events` is a long poll. The CLI waits for the first event after `since`, collects
//!   what follows within a short window and prints a page; the connector cuts the page until
//!   its reply fits one encrypted frame, deflated if the session opted in.
//! - `chat.create` changes what exists, so like `shell.create` it runs in a task that
//!   outlives the request.
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
            "title",
        ],
    )?;
    let provider = match required(object, "provider")? {
        "codex" => "codex",
        "claude" => "claude",
        _ => return Err(invalid("provider must be codex or claude")),
    };
    let target = match (
        uuid_field(object, "project_id")?,
        uuid_field(object, "worktree_id")?,
    ) {
        (Some(project), None) => CreateTarget::Project(project),
        (None, Some(worktree)) => CreateTarget::Worktree(worktree),
        _ => return Err(invalid("give exactly one of project_id and worktree_id")),
    };
    Ok(NewSpec {
        provider,
        target,
        mode: text(object, "approval_mode")?.map_or(Ok("supervised"), mode)?,
        model: label("model", text(object, "model")?, MODEL_MAX_CHARS)?,
        effort: label("effort", text(object, "effort")?, EFFORT_MAX_CHARS)?,
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
    args
}

/// A validated `chat.events`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct EventsSpec {
    chat: String,
    since: u64,
    wait_ms: i64,
    max_events: u64,
}
pub(super) fn events_spec(params: &Value) -> std::result::Result<EventsSpec, Fault> {
    let object = fields(params, &["chat_id", "since", "wait_ms", "max_events"])?;
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
    Ok(EventsSpec {
        chat,
        since,
        wait_ms: wait_ms as i64,
        max_events,
    })
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
        "interrupt" | "compact" | "stop" => &["command"],
        "approve" => &["command", "request_id", "decision"],
        "answer" => &["command", "request_id", "answers"],
        "configure" => &["command", "model", "effort", "approval_mode"],
        _ => {
            return Err(invalid(
                "command.command must be send, interrupt, approve, answer, configure, compact or stop",
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
            let mut changes = Map::from_iter([("command".to_owned(), json!("configure"))]);
            for (name, value) in [
                ("model", model.map(Value::from)),
                ("effort", effort.map(Value::from)),
                ("approval_mode", approval_mode.map(Value::from)),
            ] {
                if let Some(value) = value {
                    changes.insert(name.to_owned(), value);
                }
            }
            if changes.len() == 1 {
                return Err(invalid(
                    "configure needs a model, an effort or an approval_mode",
                ));
            }
            Value::Object(changes)
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

/// A validated `chat.options`: it takes nothing.
pub(super) fn options_spec(params: &Value) -> std::result::Result<(), Fault> {
    fields(params, &[]).map(|_| ())
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
    } else if ["codex", "claude"].into_iter().any(installed)
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
        && matches!(text("provider"), Some("codex" | "claude"))
        && text("title").is_some()
        && text("cwd").is_some()
        && value.get("created_at_unix").is_some_and(Value::is_u64)
        && value
            .get("state")
            .is_some_and(|state| state.get("state").is_some_and(|name| name.is_string()))
}
/// The `chat.create` result for the chat the CLI printed, or `None` if it is not the chat
/// that was asked for: the provider, the project or worktree, the mode, and the model and
/// effort when they were sent.
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
        && same("effort", &spec.effort))
    .then(|| json!({ "chat": cli }))
}

/// The most models or efforts one provider's list of `chat.options` may hold.
const OPTIONS_MAX: usize = 64;

/// The `chat.options` result for what the CLI printed, or `None` if it is not that: for each
/// of `codex` and `claude` that it names, `models` and `efforts` as lists of names that a
/// `configure` could send (one line, not blank, within the limits). Only those two providers
/// and those two lists are passed on: a newer CLI may print more, and the phone is told what
/// the contract says.
fn options_result(cli: &Value) -> Option<Value> {
    let providers = cli.get("providers")?.as_object()?;
    let names = |entry: &Value, list: &str, max: usize| -> Option<Value> {
        let names = entry.get(list)?.as_array()?;
        let fits = names.len() <= OPTIONS_MAX
            && names.iter().all(|name| {
                name.as_str().is_some_and(
                    |name| matches!(label(list, Some(name), max), Ok(Some(kept)) if kept == name),
                )
            });
        fits.then(|| Value::Array(names.clone()))
    };
    let mut kept = Map::new();
    for provider in ["codex", "claude"] {
        let Some(entry) = providers.get(provider) else {
            continue;
        };
        kept.insert(
            provider.to_owned(),
            json!({
                "models": names(entry, "models", MODEL_MAX_CHARS)?,
                "efforts": names(entry, "efforts", EFFORT_MAX_CHARS)?,
            }),
        );
    }
    Some(json!({ "providers": kept }))
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
            if seq <= last || !event.get("event").is_some_and(Value::is_string) {
                return None;
            }
            last = seq;
            events.push(json!({"seq": seq, "event": event}));
        }
        let next = object.get("next")?.as_u64()?;
        let more = object.get("more")?.as_bool()?;
        // Not before the last event it returned, and a page that says there is more must have
        // moved the phone on, or it would ask for the same page again.
        if next < last || (more && next == spec.since) {
            return None;
        }
        Some(Self {
            chat: spec.chat.clone(),
            events,
            next,
            more,
        })
    }
    /// The seq of the last event held, `None` for none.
    fn last_seq(&self) -> Option<u64> {
        self.events
            .last()
            .and_then(|entry| entry.get("seq"))
            .and_then(Value::as_u64)
    }
}
/// The smallest a string is cut to when an event alone is more than a frame.
const MIN_CUT: usize = 128;
/// What a sealed frame keeps free beyond what the check sees: `server_ms` is added with its
/// real digits only when the reply is sealed.
const FRAME_MARGIN: usize = 64;

/// Cuts every string longer than `cap` bytes down to it, at a character boundary, and marks
/// the cut with an ellipsis. The same cut the CLI makes of an event too big for a page.
fn cut_strings(value: &mut Value, cap: usize) {
    match value {
        Value::String(text) if text.len() > cap => {
            let mut end = cap;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push('\u{2026}');
        }
        Value::Array(items) => items.iter_mut().for_each(|item| cut_strings(item, cap)),
        Value::Object(fields) => fields.values_mut().for_each(|item| cut_strings(item, cap)),
        _ => {}
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
/// The `chat.events` result for `page`, cut until it fits one sealed frame (see `sealed`).
///
/// - A page that is too big loses its last half, and says `more`; `next` is the last event it
///   keeps. A phone never gets `response_too_large` because of how many events there were.
/// - One event that is more than a frame by itself (the CLI only keeps a page within the
///   size of a *reply*, which a session that deflates puts at 2 MiB, and noise does not
///   deflate) has its long strings cut, as the CLI cuts them for a page, until it fits. If
///   no cut helps it is passed over: the page is empty and `next` is beyond it, so the phone
///   goes on.
fn fit_page(request: &str, mut page: Page, compress: bool) -> std::result::Result<Value, Fault> {
    loop {
        if let Some(result) = sealed(request, &page, compress)? {
            return Ok(result);
        }
        if page.events.len() > 1 {
            page.events.truncate(page.events.len() / 2);
            page.next = page.last_seq().unwrap_or(page.next);
            page.more = true;
            continue;
        }
        let Some(entry) = page.events.pop() else {
            return Err(Fault::new(
                "response_too_large",
                "the reply exceeds the encrypted response limit",
            ));
        };
        let mut cap = 64 * 1024;
        while cap >= MIN_CUT {
            let mut shrunk = entry.clone();
            cut_strings(&mut shrunk["event"], cap);
            page.events.push(shrunk);
            if let Some(result) = sealed(request, &page, compress)? {
                return Ok(result);
            }
            page.events.pop();
            cap /= 2;
        }
    }
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
        if self.chat.load(Ordering::Relaxed) {
            return Ok(true);
        }
        let started = std::time::Instant::now();
        let Ok(_asking) = timeout(limit, self.asking_chat.lock()).await else {
            return Err(cli_fault("RiWork CLI timeout"));
        };
        if self.chat.load(Ordering::Relaxed) {
            return Ok(true);
        }
        let asked = self
            .raw_within(
                vec!["capabilities".into(), "--json".into()],
                limit.saturating_sub(started.elapsed()),
            )
            .await;
        let yes = match asked {
            Ok(data) => {
                let reply = serde_json::from_slice::<Value>(&data).unwrap_or(Value::Null);
                reply.get("v") == Some(&json!(1)) && reply.get("chat") == Some(&Value::Bool(true))
            }
            // It ran and refused the question: a CLI from before `capabilities`.
            Err(e) if e.to_string().starts_with("RiWork CLI failed") => false,
            Err(e) => return Err(cli_fault(e)),
        };
        if yes {
            self.chat.store(true, Ordering::Relaxed);
        }
        Ok(yes)
    }
    /// What `ready` announces as `features.chat`. The question is short: a CLI that does not
    /// answer within a few seconds does not delay the handshake any longer (the phone gives
    /// the whole handshake ten).
    pub async fn chat_supported(&self) -> bool {
        self.chat_known(Duration::from_secs(3))
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
    fn still_authorized(&self, device: &str) -> std::result::Result<(), Fault> {
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
        let args: Vec<String> = [
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
        let cli = self
            .read_capped(args, events_limit(spec.wait_ms), reply_limit)
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
        let page = Page::parse(&spec, &cli).ok_or_else(|| {
            cli_fault("CLI returned a page of events that does not fit the request")
        })?;
        // Deflating a page of megabytes, more than once, is not for a thread that serves others.
        let (request, compress) = (request.to_owned(), reply_limit > MAX_PLAINTEXT);
        tokio::task::spawn_blocking(move || fit_page(&request, page, compress))
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

    /// The models and efforts the desktop offers a chat of each provider, as its chat tabs list
    /// them. A plain read: the CLI prints its own lists and starts no chat host. A CLI from
    /// before the method refuses it as a usage error, which says to update.
    pub(super) async fn chat_options(&self) -> std::result::Result<Value, Fault> {
        self.require_chat().await?;
        let cli = self.read(&["chat", "options"]).await.map_err(|fault| {
            let older = fault
                .message
                .strip_prefix("RiWork CLI failed: riwork: ")
                .is_some_and(|detail| detail.starts_with("Usage: riwork chat "));
            if older {
                Fault::new(
                    "cli_error",
                    "the installed riwork CLI does not list chat models; update RiWork",
                )
            } else {
                chat_fault(fault)
            }
        })?;
        options_result(&cli)
            .ok_or_else(|| cli_fault("CLI returned chat options that do not fit the contract"))
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
        // One that cannot be cut small enough (its weight is not in strings) is passed over.
        let wide = json!({"seq": 1, "event": {"event": "x",
            "numbers": (0..60_000).map(|n| n * 7919).collect::<Vec<u64>>()}});
        let passed = fit_page(request, page(vec![wide]), false).unwrap();
        assert_eq!(passed["events"], json!([]));
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
}
