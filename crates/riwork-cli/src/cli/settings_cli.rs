//! Global, typed settings over the same locked merge/save path as the desktop.

use serde_json::{Value, json};

use super::{print_json, write_stdout};
use crate::settings::SettingsStore;

const HELP: &str = "\
riwork settings list [--json]          List every known top-level key, sorted
riwork settings get KEY [--json]       Read a typed value
riwork settings set KEY VALUE [--json]   Save and print the effective typed value
riwork settings set KEY --json JSON    The same command with structured output

Global settings live in RIWORK_HOME/settings.json, or ~/.local/share/riwork/settings.json.
Keys are top-level saved names, with no dotted paths or project overrides. VALUE is one
JSON value (quote objects for the shell), or a bare string such as detach. --json
selects output, not input; JSON input works without it too. Use -- before a value that
starts with --, or JSON-quote the string. Examples:
  riwork settings set dictation_mic true
  riwork settings set tab_close_behavior detach
  riwork settings set status_bar --json '{\"usage_mode\":\"all_accounts\"}'
Object values replace known preferences in that object; omitted fields use model defaults.
The store preserves unknown top-level fields and unchanged future values; replacing an
object has the store's existing whole-object semantics (chat_display_modes patches known
entries). Unknown keys, wrong types, ignored components and unreadable delegation values
are refused without saving. ui_text_size rounds to a tenth and clamps to 9..=24;
status_bar fills missing defaults and items. schema_version must remain 1. Every persisted
key is supported; runtime-only diagnostics are not settings. Claude billing uses the saved
boolean key claude_chat_ignores_api_key (true: subscription, false: API key).
Running desktop windows poll settings every two seconds and apply interface preferences
through their normal reload. Agent launch defaults and billing defaults affect new sessions;
existing orchestrators keep their mode, provider and model. Delegation changes reach an
orchestrator when it starts or loads its skill. No restart or GUI launch is requested here.
Plain list prints key = <compact JSON>; get and set print just the compact JSON value.
--json prints {\"v\":1,\"settings\":{...}} for list, {\"v\":1,\"key\":KEY,\"value\":...}
for get/set, or {\"error\":{\"code\",\"message\"}} with exit status 2 on refusal.
";

type Refusal = (&'static str, String);

pub(super) fn command(args: Vec<String>, json: bool) -> Result<(), String> {
    if args.is_empty()
        || matches!(args.as_slice(), [help] if matches!(help.as_str(), "help" | "--help" | "-h"))
    {
        print!("{HELP}");
        return Ok(());
    }
    match result(args) {
        Ok(value) if json => print_json(&value),
        Ok(value) => {
            if let Some(settings) = value.get("settings").and_then(Value::as_object) {
                let mut keys: Vec<_> = settings.keys().collect();
                keys.sort();
                for key in keys {
                    println!("{key} = {}", settings[key]);
                }
            } else {
                println!("{}", value["value"]);
            }
            Ok(())
        }
        Err((code, message)) if json => {
            print_json(&json!({"error":{"code":code,"message":message}}))?;
            Err(String::new())
        }
        Err((_, message)) => Err(message),
    }
}

fn result(args: Vec<String>) -> Result<Value, Refusal> {
    let invalid = |message: String| ("invalid_request", message);
    let internal = |message: String| ("cli_error", message);
    let usage = || {
        invalid("Usage: riwork settings list | get KEY | set KEY VALUE [--json]; run riwork settings help".to_owned())
    };
    let (operation, key, input) = match args.as_slice() {
        [op] if op == "list" => (op.as_str(), None, None),
        [op, key] if op == "get" && !key.starts_with('-') => {
            (op.as_str(), Some(key.as_str()), None)
        }
        [op, key, value] if op == "set" && !key.starts_with('-') && !value.starts_with("--") => {
            (op.as_str(), Some(key.as_str()), Some(value.as_str()))
        }
        [op, key, separator, value]
            if op == "set" && !key.starts_with('-') && separator == "--" =>
        {
            (op.as_str(), Some(key.as_str()), Some(value.as_str()))
        }
        _ => return Err(usage()),
    };
    let known = crate::settings::Settings::default()
        .values()
        .map_err(internal)?;
    if let Some(key) = key
        && !known.contains_key(key)
    {
        return Err(invalid(format!(
            "'{key}' is not a known setting; run riwork settings list"
        )));
    }
    let input = input.map(parse_value).transpose().map_err(invalid)?;
    let home = crate::paths::riwork_home().map_err(internal)?;
    let store = SettingsStore::open(home).map_err(internal)?;
    let settings = match (key, input) {
        (Some(key), Some(value)) => store.set_value(key, value).map_err(|message| match message
            .strip_prefix("invalid_request: ")
        {
            Some(reason) => invalid(reason.to_owned()),
            None => internal(message),
        })?,
        _ => store.load().map_err(internal)?,
    };
    // update returns the typed re-read of the merged document actually saved.
    let values = settings.values().map_err(internal)?;
    if operation == "list" {
        Ok(json!({"v":1,"settings":values}))
    } else {
        let key = key.unwrap();
        Ok(json!({"v":1,"key":key,"value":values[key]}))
    }
}

fn parse_value(raw: &str) -> Result<Value, String> {
    match serde_json::from_str(raw) {
        Ok(value) => Ok(value),
        Err(error) if raw.trim_start().starts_with(['{', '[', '"']) => {
            Err(format!("Invalid setting JSON: {error}"))
        }
        Err(_) => Ok(Value::String(raw.to_owned())),
    }
}
