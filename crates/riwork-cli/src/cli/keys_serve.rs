//! `riwork shell keys --serve`: the remote connector's typing worker.
//!
//! `riwork shell keys ID -- ITEM...` types one batch and exits, so a phone that types
//! costs a process start for every batch it sends. The connector instead keeps one
//! of these per paired device and hands it batch after batch. Each batch goes
//! through exactly what `shell keys` does: the same item parsing and limits, the
//! shell's input lock, the copy-mode and input-off checks, the pause before a key
//! that follows text, and the same error tokens. Only the process start is saved;
//! the connector keeps its ledger, its locks and its ordering as they were.
//!
//! The protocol is JSON lines. The worker first writes `{"ready":true}`, so the
//! connector knows it started before it hands it anything to type. A request is
//! one line on standard input:
//!
//! ```text
//! {"n":7,"shell":"UUID","items":["t:ls -la","k:Enter"]}
//! ```
//!
//! `items` uses the argv encoding of `shell keys` (`t:TEXT`, `k:KEY`). The answer
//! is one line on standard output, before the next request is read:
//!
//! ```text
//! {"n":7,"ok":true}
//! {"n":7,"ok":false,"error":"not_found: unknown shell UUID"}
//! ```
//!
//! `error` is exactly what `shell keys` would have printed after `riwork: `, so an
//! error that begins with a `session_keys` token happened before any key was typed,
//! and any other may have typed part of the batch. `n` is echoed as it came (null
//! when the line could not be read as a request). The worker ends at the end of its
//! input, and it never writes anything else to standard output.

use std::io::{self, BufRead, Read, Write};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{session_keys, sessions::SessionManager};

/// A request line longer than this is refused and ends the worker: the
/// connector never writes one (64 items and 4096 text bytes, escaped, fit far
/// below it), so the stream is not what it should be.
const MAX_LINE_BYTES: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    n: u64,
    shell: String,
    items: Vec<String>,
}

/// Types every batch read from standard input, answering each on standard output.
pub(super) fn serve(manager: &SessionManager) -> Result<(), String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", json!({"ready": true}))
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("cannot answer: {error}"))?;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = Read::take(&mut input, MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("cannot read a request: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        if line.last() != Some(&b'\n') && line.len() as u64 > MAX_LINE_BYTES {
            return Err(format!(
                "{}a request line is longer than {MAX_LINE_BYTES} bytes",
                session_keys::INVALID_REQUEST
            ));
        }
        let answer = answer(manager, &line);
        writeln!(stdout, "{answer}")
            .and_then(|()| stdout.flush())
            .map_err(|error| format!("cannot answer: {error}"))?;
    }
}

/// The answer to one request line.
fn answer(manager: &SessionManager, line: &[u8]) -> Value {
    let request = match serde_json::from_slice::<Request>(line) {
        Ok(request) => request,
        Err(error) => {
            let n = serde_json::from_slice::<Value>(line)
                .ok()
                .and_then(|value| value.get("n").cloned())
                .unwrap_or(Value::Null);
            return json!({
                "n": n,
                "ok": false,
                "error": format!("{}malformed request: {error}", session_keys::INVALID_REQUEST),
            });
        }
    };
    let result = session_keys::parse_items(&request.items)
        .map_err(|error| format!("{}{error}", session_keys::INVALID_REQUEST))
        .and_then(|items| manager.send_keys(&request.shell, &items));
    match result {
        Ok(()) => json!({"n": request.n, "ok": true}),
        Err(error) => json!({"n": request.n, "ok": false, "error": error}),
    }
}
