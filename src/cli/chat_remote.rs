//! The chat commands a phone reaches through the remote connector
//! (`docs/remote-protocol.md`, "Chat extension"): `chat events`, which follows
//! a chat for a while and prints what arrived as one bounded JSON line, and
//! `chat command`, which sends one strictly checked `ChatCommand`. The
//! connector runs them with an argv and reads their JSON, so what they print
//! and how they refuse is part of that protocol.

use crate::chat::{
    client::{Poll, Subscription},
    model::ChatCommand,
    wire::Envelope,
};
use serde::Serialize;
use serde_json::Value;
use std::time::{Duration, Instant};

/// The most `--wait-ms` may ask for.
pub(super) const MAX_WAIT_MS: u64 = 25_000;
/// Events one page holds at most, and by default.
const MAX_EVENTS: usize = 2000;
const DEFAULT_EVENTS: usize = 500;
/// The bytes of the printed line: its bounds and its default.
const MIN_BYTES: usize = 1024;
const MAX_BYTES: usize = 2 << 20;
const DEFAULT_BYTES: usize = 1 << 20;
/// A page keeps collecting this long after its first event, so that a burst of
/// events (a message streaming in) travels together.
const BATCH_WINDOW: Duration = Duration::from_millis(50);
/// How long the replay of a chat's log may take to start. A page that asks for
/// no wait still finds the events that are there.
pub(super) const REPLAY_GRACE: Duration = Duration::from_millis(100);
/// How long a page that is full looks for one more event, to know whether
/// there is more.
const PEEK: Duration = Duration::from_millis(2);
/// The smallest a string is cut to when an event is too big for a page alone.
const MIN_CUT: usize = 256;
/// The longest message `chat command` sends, in bytes.
pub(super) const MAX_TEXT: usize = 64 * 1024;
const MAX_MODEL_CHARS: usize = 100;
const MAX_EFFORT_CHARS: usize = 32;
const MAX_TITLE_CHARS: usize = 200;

pub(super) const EVENTS_USAGE: &str = "Usage: riwork chat events CHAT_ID [--since N] [--wait-ms N] [--max N] [--max-bytes N] [--json]";
pub(super) const COMMAND_USAGE: &str =
    "Usage: riwork chat command CHAT_ID (--command-json JSON | -- JSON) [--json]";

/// A refusal of what the caller sent, as the connector tells it from a failure
/// of the host: it starts with this token.
pub(super) fn invalid(message: impl std::fmt::Display) -> String {
    format!("invalid_request: {message}")
}

// ---- Options -------------------------------------------------------------------------------

/// Takes `--option VALUE` or `--option=VALUE` out of `args`, the value as it
/// is, even if it begins with `--` (a title may). At most once.
pub(super) fn take_verbatim_option(
    args: &mut Vec<String>,
    option: &str,
) -> Result<Option<String>, String> {
    let prefix = format!("{option}=");
    let mut result = None;
    let mut index = 0;
    while index < args.len() {
        let value = if args[index] == option {
            if index + 1 >= args.len() {
                return Err(format!("{option} needs a value"));
            }
            args.remove(index);
            args.remove(index)
        } else if let Some(value) = args[index].strip_prefix(&prefix) {
            let value = value.to_owned();
            args.remove(index);
            value
        } else {
            index += 1;
            continue;
        };
        if result.replace(value).is_some() {
            return Err(format!("{option} can only be given once"));
        }
    }
    Ok(result)
}

fn plain_text(name: &str, text: &str) -> Result<(), String> {
    if text
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        return Err(format!("{name} must not contain control characters"));
    }
    Ok(())
}

fn bounded(name: &str, text: String, max_chars: usize) -> Result<String, String> {
    plain_text(name, &text)?;
    if text.chars().count() > max_chars {
        return Err(format!("{name} must be at most {max_chars} characters"));
    }
    Ok(text)
}

/// A model name: not blank, at most 100 characters.
pub(super) fn model_setting(name: &str, text: String) -> Result<String, String> {
    if text.trim().is_empty() {
        return Err(format!("{name} must not be blank"));
    }
    bounded(name, text, MAX_MODEL_CHARS)
}

/// A reasoning effort: not blank, at most 32 characters.
pub(super) fn effort_setting(name: &str, text: String) -> Result<String, String> {
    if text.trim().is_empty() {
        return Err(format!("{name} must not be blank"));
    }
    bounded(name, text, MAX_EFFORT_CHARS)
}

/// A chat title: at most 200 characters; a blank one is none.
pub(super) fn title_setting(name: &str, text: String) -> Result<Option<String>, String> {
    let text = bounded(name, text, MAX_TITLE_CHARS)?;
    Ok((!text.trim().is_empty()).then_some(text))
}

/// The options of `chat events`, checked.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct EventsArguments {
    pub chat: String,
    pub since: u64,
    pub wait: Duration,
    pub max: usize,
    pub max_bytes: usize,
}

pub(super) fn number(
    args: &mut Vec<String>,
    option: &str,
    default: u64,
    range: std::ops::RangeInclusive<u64>,
) -> Result<u64, String> {
    let Some(text) = super::take_option(args, option)? else {
        return Ok(default);
    };
    text.parse::<u64>()
        .ok()
        .filter(|value| range.contains(value))
        .ok_or_else(|| {
            format!(
                "{option} must be a whole number from {} to {}",
                range.start(),
                range.end()
            )
        })
}

pub(super) fn parse_events_arguments(mut args: Vec<String>) -> Result<EventsArguments, String> {
    let since = number(&mut args, "--since", 0, 0..=u64::MAX)?;
    let wait = number(&mut args, "--wait-ms", 0, 0..=MAX_WAIT_MS)?;
    let max = number(
        &mut args,
        "--max",
        DEFAULT_EVENTS as u64,
        1..=MAX_EVENTS as u64,
    )?;
    let max_bytes = number(
        &mut args,
        "--max-bytes",
        DEFAULT_BYTES as u64,
        MIN_BYTES as u64..=MAX_BYTES as u64,
    )?;
    if args.len() != 1 || args[0].starts_with("--") {
        return Err(EVENTS_USAGE.to_owned());
    }
    let chat = args.remove(0);
    Ok(EventsArguments {
        chat,
        since,
        wait: Duration::from_millis(wait),
        max: max as usize,
        max_bytes: max_bytes as usize,
    })
}

// ---- Collecting a page ---------------------------------------------------------------------

/// Where a page's events come from: a chat's subscription, or a script.
pub(super) trait Source {
    fn next(&mut self, wait: Duration) -> Result<Poll, String>;
}

impl Source for Subscription {
    fn next(&mut self, wait: Duration) -> Result<Poll, String> {
        self.next_within(wait)
    }
}

/// One entry of a page.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct Entry {
    pub seq: u64,
    pub event: Value,
}

/// What `chat events` prints.
#[derive(Debug, PartialEq, Serialize)]
pub(super) struct Page {
    pub chat_id: String,
    pub events: Vec<Entry>,
    /// The `since` of the next call.
    pub next: u64,
    /// The page was cut short, with events left to read.
    pub more: bool,
}

impl Page {
    /// The compact JSON line, newline included.
    pub fn line(&self) -> Result<String, String> {
        serde_json::to_string(self)
            .map(|line| line + "\n")
            .map_err(|error| error.to_string())
    }

    /// One `{"seq":..,"event":..}` line per event.
    pub fn event_lines(&self) -> Result<String, String> {
        let mut text = String::new();
        for entry in &self.events {
            text += &serde_json::to_string(entry).map_err(|error| error.to_string())?;
            text.push('\n');
        }
        Ok(text)
    }
}

/// What a page may hold.
pub(super) struct Plan {
    pub chat_id: String,
    pub since: u64,
    /// When the first event must have arrived, or the page is empty.
    pub first_by: Instant,
    pub max: usize,
    /// The most bytes of the printed line.
    pub max_bytes: usize,
}

/// The printed line without any event, as long as it can get.
fn fixed_bytes(chat_id: &str) -> usize {
    serde_json::to_string(&Page {
        chat_id: chat_id.to_owned(),
        events: Vec::new(),
        next: u64::MAX,
        more: false,
    })
    .map_or(0, |line| line.len())
}

fn entry_bytes(entry: &Entry) -> usize {
    // The comma that goes before it is counted too.
    serde_json::to_string(entry).map_or(usize::MAX, |text| text.len() + 1)
}

/// Cuts every string longer than `cap` bytes down to it, at a character
/// boundary, and marks the cut with an ellipsis.
pub(super) fn cut_strings(value: &mut Value, cap: usize) {
    if value["kind"].as_str() == Some("data")
        && value["base64"]
            .as_str()
            .is_some_and(|data| data.len() > cap)
    {
        *value = serde_json::json!({"kind": "unavailable", "reason": "Image omitted to fit the remote response limit"});
        return;
    }
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

/// An event that is too big for a page alone, with its strings cut until it
/// fits; `None` when no cut makes it fit.
pub(super) fn shrink(entry: &Entry, budget: usize) -> Option<Entry> {
    let mut cap = (budget / 4).max(MIN_CUT);
    loop {
        let mut event = entry.event.clone();
        cut_strings(&mut event, cap);
        let shrunk = Entry {
            seq: entry.seq,
            event,
        };
        if entry_bytes(&shrunk) <= budget {
            return Some(shrunk);
        }
        if cap <= MIN_CUT {
            return None;
        }
        cap = (cap / 2).max(MIN_CUT);
    }
}

/// Exceptional recovery may shorten bodies, never identity, order or actionable controls.
fn shorten_body(event: &mut Value, cap: usize) -> bool {
    match event["event"].as_str() {
        Some("item_started" | "item_completed") => {
            cut_strings(&mut event["item"]["body"], cap);
            if cap == MIN_CUT {
                event["item"]["body"] = serde_json::json!({"type":"agent_message", "text":"This message is too long to show here. Full text is on your Mac."});
            }
            true
        }
        Some("item_delta") => {
            cut_strings(&mut event["delta"], cap);
            true
        }
        _ => false,
    }
}
fn shrink_body(entry: &Entry, budget: usize) -> Option<Entry> {
    let mut cap = (budget / 4).max(MIN_CUT);
    loop {
        let mut shrunk = entry.clone();
        if !shorten_body(&mut shrunk.event, cap) {
            return None;
        }
        if entry_bytes(&shrunk) <= budget {
            return Some(shrunk);
        }
        if cap == MIN_CUT {
            return None;
        }
        cap = (cap / 2).max(MIN_CUT);
    }
}

/// Collects a page from `source`.
///
/// - The first event is waited for until `plan.first_by`; none by then is an
///   empty page.
/// - From the first event on, events are collected for 50 ms, so a burst
///   travels together. A page stops early when it holds `plan.max` events or
///   the next one would push the printed line past `plan.max_bytes`; that event
///   is left unread, and `more` says so.
/// - An event that alone is bigger than the page is shrunk (see `shrink`), or
///   dropped when nothing helps, so the page can always move on: it counts
///   as read.
pub(super) fn collect(source: &mut impl Source, plan: &Plan) -> Result<Page, String> {
    collect_page(source, plan, false, false)
}
pub(super) fn collect_complete(source: &mut impl Source, plan: &Plan) -> Result<Page, String> {
    collect_page(source, plan, true, false)
}
pub(super) fn collect_bounded(source: &mut impl Source, plan: &Plan) -> Result<Page, String> {
    collect_page(source, plan, false, true)
}
fn collect_page(
    source: &mut impl Source,
    plan: &Plan,
    complete: bool,
    bounded: bool,
) -> Result<Page, String> {
    let budget = plan.max_bytes.saturating_sub(fixed_bytes(&plan.chat_id));
    let mut page = Page {
        chat_id: plan.chat_id.clone(),
        events: Vec::new(),
        next: plan.since,
        more: false,
    };
    let (mut used, mut read) = (0usize, 0usize);
    let mut window_end: Option<Instant> = None;
    loop {
        let now = Instant::now();
        let wait = match window_end {
            None => plan.first_by.saturating_duration_since(now),
            Some(_) if read >= plan.max => PEEK,
            Some(end) => end.saturating_duration_since(now),
        };
        if window_end.is_some() && wait.is_zero() {
            break;
        }
        match source.next(wait)? {
            Poll::TimedOut => break,
            Poll::Closed => {
                if read == 0 {
                    return Err("chat host closed the connection".into());
                }
                break;
            }
            Poll::Event(envelope) => {
                let Envelope { seq, event, .. } = envelope;
                if seq <= page.next {
                    return Err(format!(
                        "chat host sent event {seq} after {}, out of order",
                        page.next
                    ));
                }
                if read >= plan.max {
                    page.more = true;
                    break;
                }
                let event = serde_json::to_value(&event).map_err(|error| error.to_string())?;
                let mut entry = Entry { seq, event };
                if used + entry_bytes(&entry) > budget {
                    if !page.events.is_empty() {
                        page.more = true;
                        break;
                    }
                    if complete {
                        return Err(
                            "response_too_large: a complete event exceeds the response limit"
                                .into(),
                        );
                    }
                    let fitted = if bounded {
                        shrink_body(&entry, budget)
                    } else {
                        shrink(&entry, budget)
                    };
                    match fitted {
                        Some(shrunk) => entry = shrunk,
                        None => {
                            if bounded {
                                return Err("response_too_large: event cannot be represented without losing identity or controls".into());
                            }
                            // Hopeless: read past it.
                            window_end.get_or_insert(Instant::now() + BATCH_WINDOW);
                            read += 1;
                            page.next = seq;
                            continue;
                        }
                    }
                }
                window_end.get_or_insert(Instant::now() + BATCH_WINDOW);
                used += entry_bytes(&entry);
                read += 1;
                page.next = seq;
                page.events.push(entry);
            }
        }
    }
    Ok(page)
}

// ---- chat command ----------------------------------------------------------------------------

/// The fields of `Configure` that may be left out, and so may be `null`. No other field of
/// any command may be `null`: that is a field the command does not have.
const OPTIONAL_FIELDS: [&str; 4] = ["model", "effort", "approval_mode", "fast"];
/// The same for `Switch`, whose `provider` is required.
const SWITCH_OPTIONAL_FIELDS: [&str; 3] = ["model", "effort", "fast"];

/// Whether everything in `input` was taken into `output`, the command serde made of it: a
/// field serde does not know is not there, and one it had to ignore differs. A `null` is no
/// value, but only for a field of `nullable` (at the top, not inside a value).
fn consumed(input: &Value, output: &Value, nullable: &[&str]) -> bool {
    match (input, output) {
        (Value::Object(given), Value::Object(taken)) => given.iter().all(|(name, value)| {
            if value.is_null() {
                nullable.contains(&name.as_str()) && !taken.contains_key(name)
            } else {
                taken.get(name).is_some_and(|t| consumed(value, t, &[]))
            }
        }),
        (Value::Array(given), Value::Array(taken)) => {
            given.len() == taken.len() && given.iter().zip(taken).all(|(g, t)| consumed(g, t, &[]))
        }
        _ => input == output,
    }
}

/// What is wrong with a command serde could not read, without the text of
/// what was sent: serde's own sentence quotes a value that has the wrong kind.
/// Only the sentences that name a missing field or an unknown command are kept.
fn shape_error(error: &serde_json::Error) -> String {
    let said = error.to_string();
    if said.starts_with("missing field") || said.starts_with("unknown variant") {
        let said: String = said
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(120)
            .collect();
        invalid(format!("the command is not one the chat takes: {said}"))
    } else {
        invalid("the command has a field of the wrong kind")
    }
}

/// One `ChatCommand` from the JSON text of `chat command`, strictly: an object
/// of a known command, no field it does not use, and sane values. A refusal
/// never repeats a message's text.
pub(super) fn parse_command(text: &str) -> Result<ChatCommand, String> {
    let value: Value = serde_json::from_str(text).map_err(|error| {
        invalid(format!(
            "the command is not valid JSON (line {}, column {})",
            error.line(),
            error.column()
        ))
    })?;
    if !value.is_object() {
        return Err(invalid("the command must be a JSON object"));
    }
    let command: ChatCommand =
        serde_json::from_value(value.clone()).map_err(|e| shape_error(&e))?;
    let taken = serde_json::to_value(&command).map_err(|error| error.to_string())?;
    let nullable: &[&str] = match command {
        ChatCommand::Configure { .. } => &OPTIONAL_FIELDS,
        ChatCommand::Switch { .. } => &SWITCH_OPTIONAL_FIELDS,
        _ => &[],
    };
    if !consumed(&value, &taken, nullable) {
        return Err(invalid(
            "the command has a field it does not use, or one of the wrong kind",
        ));
    }
    match &command {
        ChatCommand::Send { text } => {
            if text.trim().is_empty() {
                return Err(invalid("a message must not be blank"));
            }
            if text.len() > MAX_TEXT {
                return Err(invalid(format!(
                    "a message must be at most {MAX_TEXT} bytes"
                )));
            }
        }
        ChatCommand::Configure {
            model,
            effort,
            approval_mode,
            fast,
        } => {
            if model.is_none() && effort.is_none() && approval_mode.is_none() && fast.is_none() {
                return Err(invalid(
                    "configure needs a model, an effort, an approval_mode or fast",
                ));
            }
            if let Some(model) = model {
                model_setting("model", model.clone()).map_err(invalid)?;
            }
            if let Some(effort) = effort {
                effort_setting("effort", effort.clone()).map_err(invalid)?;
            }
        }
        ChatCommand::Switch { model, effort, .. } => {
            if let Some(model) = model {
                model_setting("model", model.clone()).map_err(invalid)?;
            }
            if let Some(effort) = effort {
                effort_setting("effort", effort.clone()).map_err(invalid)?;
            }
        }
        _ => {}
    }
    Ok(command)
}
