//! `riwork handoff`: pass a conversation from a shell or a chat to a new shell or chat.
//! The work is `crate::handoff`; this reads the command line and the caller's own session
//! and prints what was started.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::{
    effort_setting, ensure_empty, json_text, model_setting, take_verbatim_option, write_stdout,
};
use crate::{
    chat::model::ApprovalMode,
    codex_accounts::CodexAccountBinding,
    handoff::{self, Context, Env, Kind, Request, Started, summary::Timing},
    sessions::HarnessKind,
};

pub(super) const USAGE: &str = "Usage: riwork handoff [--from SHELL_OR_CHAT_ID] --to shell|chat --provider codex|claude|grok [--model NAME] [--effort LEVEL] [--account LABEL_OR_ID] [--mode supervised|auto-edit|full|plan] [--context transcript|summary] [--note TEXT] [--json]";

/// The longest note, in characters.
const MAX_NOTE_CHARS: usize = 2000;

/// A command line, read.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Arguments {
    pub from: Option<String>,
    pub kind: Kind,
    pub provider: HarnessKind,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account: Option<String>,
    pub mode: Option<ApprovalMode>,
    pub context: Context,
    pub note: Option<String>,
}

pub(super) fn parse(mut args: Vec<String>) -> Result<Arguments, String> {
    // Free text first: a note or a model may look like an option.
    let note = take_verbatim_option(&mut args, "--note")?
        .map(note_setting)
        .transpose()?
        .flatten();
    let model = take_verbatim_option(&mut args, "--model")?
        .map(|model| model_setting("--model", model))
        .transpose()?;
    let effort = take_verbatim_option(&mut args, "--effort")?
        .map(|effort| effort_setting("--effort", effort))
        .transpose()?;
    let account = take_verbatim_option(&mut args, "--account")?
        .map(|account| account_setting("--account", account))
        .transpose()?;
    let from = take_verbatim_option(&mut args, "--from")?;
    let kind = match take_verbatim_option(&mut args, "--to")?.as_deref() {
        Some("shell") => Kind::Shell,
        Some("chat") => Kind::Chat,
        _ => return Err(format!("--to must be shell or chat\n{USAGE}")),
    };
    let provider = match take_verbatim_option(&mut args, "--provider")?.as_deref() {
        Some("codex") => HarnessKind::Codex,
        Some("claude") => HarnessKind::Claude,
        Some("grok") => HarnessKind::Grok,
        _ => return Err(format!("--provider must be codex, claude or grok\n{USAGE}")),
    };
    let mode = match take_verbatim_option(&mut args, "--mode")?.as_deref() {
        None => None,
        Some("supervised") => Some(ApprovalMode::Supervised),
        Some("auto-edit") => Some(ApprovalMode::AutoEdit),
        Some("full") => Some(ApprovalMode::Full),
        Some("plan") => Some(ApprovalMode::Plan),
        Some(_) => return Err("--mode must be supervised, auto-edit, full, or plan".into()),
    };
    let context = match take_verbatim_option(&mut args, "--context")?.as_deref() {
        None | Some("transcript") => Context::Transcript,
        Some("summary") => Context::Summary,
        Some(_) => return Err("--context must be transcript or summary".into()),
    };
    ensure_empty(&args)?;
    Ok(Arguments {
        from,
        kind,
        provider,
        model,
        effort,
        account,
        mode,
        context,
        note,
    })
}

/// A note is one line of text (see `handoff::tidy_note`).
fn note_setting(text: String) -> Result<Option<String>, String> {
    if text.chars().any(|c| c.is_control() && !c.is_whitespace()) {
        return Err("--note must not contain control characters".into());
    }
    if text.chars().count() > MAX_NOTE_CHARS {
        return Err(format!(
            "--note must be at most {MAX_NOTE_CHARS} characters"
        ));
    }
    Ok(handoff::tidy_note(&text))
}

fn account_setting(name: &str, text: String) -> Result<String, String> {
    let account = text.trim();
    if account.is_empty() || account.chars().count() > 200 || account.chars().any(char::is_control)
    {
        return Err(format!(
            "{name} must be an account label or id of at most 200 characters"
        ));
    }
    Ok(account.to_owned())
}

/// Where a `riwork handoff` runs: the data directory, the caller's own session, and the
/// things it reaches out to.
pub(super) struct Caller<'a> {
    pub home: &'a Path,
    /// The caller's `RIWORK_SHELL_ID` and `RIWORK_CHAT_ID`, which say whose conversation
    /// it is when `--from` does not.
    pub shell_env: Option<&'a str>,
    pub chat_env: Option<&'a str>,
    /// Starts the chat host and says where it listens.
    pub ensure: &'a dyn Fn() -> Result<PathBuf, String>,
    pub account: &'a dyn Fn(&Path, &str) -> Result<CodexAccountBinding, String>,
    pub timing: Timing,
}

/// What `riwork handoff` prints.
pub(super) fn output(caller: &Caller<'_>, args: Vec<String>, json: bool) -> Result<String, String> {
    let arguments = parse(args)?;
    let selector =
        handoff::source_selector(arguments.from.as_deref(), caller.shell_env, caller.chat_env)?;
    let source = handoff::resolve_source(caller.home, &selector)?;
    let request = Request {
        source,
        kind: arguments.kind,
        provider: arguments.provider,
        model: arguments.model,
        effort: arguments.effort,
        account: arguments.account,
        mode: arguments.mode,
        context: arguments.context,
        note: arguments.note,
    };
    let env = Env {
        home: caller.home,
        ensure: caller.ensure,
        account: caller.account,
        timing: caller.timing,
    };
    let outcome = handoff::run(&env, request, &|step| eprintln!("{step}"))?;
    let (kind, id) = match &outcome.target {
        Started::Shell(shell) => ("shell", shell.id.clone()),
        Started::Chat(chat) => ("chat", chat.id.clone()),
    };
    if json {
        let mut value = json!({
            "handoff_id": outcome.handoff_id,
            "document": outcome.document,
            "target": { "kind": kind, "id": id },
            "context": match outcome.context {
                Context::Transcript => "transcript",
                Context::Summary => "summary",
            },
        });
        if let Some(reason) = &outcome.fallback {
            value["fallback"] = json!(reason);
        }
        return json_text(&value);
    }
    let mut text = format!(
        "Handed off to a {} {kind}: {id}\nThe handoff is in {}\n",
        handoff::agent_name(arguments.provider),
        outcome.document.display()
    );
    if let Some(reason) = &outcome.fallback {
        text.push_str(&format!(
            "No summary was had ({reason}), so the handoff holds the transcript.\n"
        ));
    }
    Ok(text)
}

/// `riwork handoff` as the command line runs it.
pub(super) fn command(args: Vec<String>, json: bool) -> Result<(), String> {
    let home = crate::paths::riwork_home()?;
    let variable = |name: &str| match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(format!("{name} is not valid UTF-8")),
    };
    let (shell_env, chat_env) = (variable("RIWORK_SHELL_ID")?, variable("RIWORK_CHAT_ID")?);
    let ensure_home = home.clone();
    let ensure = move || {
        let executable =
            std::env::current_exe().map_err(|error| format!("Cannot locate RiWork: {error}"))?;
        crate::chat::host::ensure(&ensure_home, &executable)
    };
    let caller = Caller {
        home: &home,
        shell_env: shell_env.as_deref(),
        chat_env: chat_env.as_deref(),
        ensure: &ensure,
        account: &crate::codex_accounts::resolve_account,
        timing: Timing::DEFAULT,
    };
    let text = output(&caller, args, json)?;
    print!("{text}");
    Ok(())
}

#[cfg(test)]
mod tests;
