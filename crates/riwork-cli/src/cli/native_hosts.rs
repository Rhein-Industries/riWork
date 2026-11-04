//! The one native bridge method available without a desktop runtime. Keep the
//! request/response contract identical to the desktop's `native invoke` bridge.

use serde_json::{Value, json};
use std::io::Read;

const MAX_ARGS: usize = 256 * 1024;
const MAX_RESPONSE: usize = 2 * 1024 * 1024;

pub(super) fn command(args: &[String], json: bool) -> Result<(), String> {
    let [invoke, target] = args else {
        return Err(usage());
    };
    if invoke != "invoke" || !json {
        return Err(usage());
    }
    let answer = if target != "Hosts.local" {
        failure("invalid_request", "unsupported native method")
    } else {
        match read_args(std::io::stdin().lock()) {
            Ok(()) => match local() {
                Ok(value) => json!({"v": 1, "value": value}),
                Err((code, message)) => failure(code, &message),
            },
            Err((code, message)) => failure(code, &message),
        }
    };
    super::write_stdout(format_args!("{answer}\n"));
    Ok(())
}

fn usage() -> String {
    "Usage: somtus native invoke Hosts.local --json < ARGS".into()
}

fn failure(code: &str, message: &str) -> Value {
    json!({"v": 1, "error": {"code": code, "message": message}})
}

fn read_args(input: impl Read) -> Result<(), (&'static str, String)> {
    let mut bytes = Vec::new();
    input
        .take(MAX_ARGS as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            (
                "cli_error",
                format!("io: cannot read the arguments: {error}"),
            )
        })?;
    if bytes.len() > MAX_ARGS {
        return Err((
            "invalid_request",
            format!("the arguments are longer than {MAX_ARGS} bytes"),
        ));
    }
    // The desktop also treats empty/whitespace standard input as {}.
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|error| ("invalid_request", error.to_string()))?;
    match value {
        Value::Object(args) if args.is_empty() => Ok(()),
        Value::Object(_) => Err((
            "invalid_request",
            "Hosts.local does not accept arguments".into(),
        )),
        _ => Err((
            "invalid_request",
            "the arguments must be a JSON object".into(),
        )),
    }
}

fn local() -> Result<Value, (&'static str, String)> {
    let backend_error = |message: String| ("cli_error", message);
    let home = crate::paths::riwork_home().map_err(backend_error)?;
    let snapshot = riwork_engine::host_sessions::local(&home).map_err(backend_error)?;
    let value =
        serde_json::to_value(snapshot).map_err(|error| ("cli_error", format!("io: {error}")))?;
    if serde_json::to_vec(&value).map_or(usize::MAX, |bytes| bytes.len()) > MAX_RESPONSE {
        // Returned separately so the native bridge's response-too-large code is
        // retained even if future metadata grows beyond the current row bounds.
        return Err((
            "response_too_large",
            "The host session snapshot exceeds the response limit".into(),
        ));
    }
    Ok(value)
}
