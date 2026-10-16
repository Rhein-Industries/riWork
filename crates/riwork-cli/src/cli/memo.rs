//! `riwork project memo show|set ID`: a project's memo (`riwork_core::project_memo`), the
//! person's own Markdown notes for it. `set` reads the whole new text from standard input, so
//! it never appears in a process list, and replaces the memo: the last save wins.
//!
//! With `--json` both print the memo as the remote connector's `project.memo.get` and
//! `project.memo.set` pass it on, `{"v":1,"project_id","text","updated_at"?}` (`updated_at`
//! left out for a memo never saved), or, when refused, `{"error":{"code","message"}}` with the
//! code `invalid_request`, `not_found` or `cli_error`.

use std::io::{self, IsTerminal, Read};

use serde_json::{Value, json};

use super::{ensure_empty, pop_command, print_json, take_single, write_stdout};
use crate::{project_memo, store::Store};

pub(super) const USAGE: &str =
    "Usage: riwork project memo show ID [--json], or riwork project memo set ID [--json] < TEXT";

/// The most standard input `set` reads: the cap with room for line breaks and controls that
/// normalization removes. Anything longer is refused unread.
const MAX_INPUT_BYTES: usize = 4 * project_memo::MAX_BYTES;

type Refusal = (&'static str, String);

pub(super) fn command(
    store: &Store,
    args: Vec<String>,
    json: bool,
    input: &mut impl Read,
) -> Result<(), String> {
    match result(store, args, input) {
        Ok((value, text, saved)) => {
            if json {
                print_json(&value)
            } else if saved {
                println!(
                    "Saved the memo of project {} ({} bytes)",
                    value["project_id"].as_str().unwrap_or_default(),
                    text.len()
                );
                Ok(())
            } else {
                // Exactly the memo, so `show > file` and `set < file` round-trip it; only a
                // terminal gets a line break after one that lacks it, for the prompt.
                print!("{text}");
                if !text.is_empty() && !text.ends_with('\n') && io::stdout().is_terminal() {
                    println!();
                }
                Ok(())
            }
        }
        Err((code, message)) if json => {
            print_json(&json!({"error":{"code":code,"message":message}}))
        }
        Err((_, message)) => Err(message),
    }
}

fn result(
    store: &Store,
    mut args: Vec<String>,
    input: &mut impl Read,
) -> Result<(Value, String, bool), Refusal> {
    let invalid = |message: String| ("invalid_request", message);
    let operation = pop_command(&mut args, "");
    let (project_id, memo, saved) = match operation.as_str() {
        "show" => {
            let selector = take_single(args, USAGE).map_err(invalid)?;
            let state = store.snapshot().map_err(|e| ("cli_error", e))?;
            let project_id = state.project(&selector).map_err(found)?.id.clone();
            let memo = state.project_memo(&project_id);
            (project_id, memo, false)
        }
        "set" => {
            let selector = take_single(args, USAGE).map_err(invalid)?;
            let text = read_text(input)?;
            let (project_id, memo) =
                store.set_project_memo(&selector, &text).map_err(|e| {
                    match e.strip_prefix("too_large: ") {
                        Some(message) => invalid(message.to_owned()),
                        None => found(e),
                    }
                })?;
            (project_id, memo, true)
        }
        _ => {
            ensure_empty(&args).map_err(invalid)?;
            return Err(invalid(USAGE.to_owned()));
        }
    };
    let mut value = json!({"v": 1, "project_id": project_id, "text": memo.text});
    if memo.updated_at > 0 {
        value["updated_at"] = json!(memo.updated_at);
    }
    Ok((value, memo.text, saved))
}

/// A store error about which project was meant is `not_found`; anything else is the store's.
fn found(error: String) -> Refusal {
    if error.starts_with("No project matches") || error.starts_with("More than one project") {
        ("not_found", error)
    } else {
        ("cli_error", error)
    }
}

fn read_text(input: &mut impl Read) -> Result<String, Refusal> {
    let mut bytes = Vec::new();
    input
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ("cli_error", format!("Cannot read the memo: {e}")))?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err((
            "invalid_request",
            format!(
                "The memo is too long; a memo holds at most {} bytes (32 KB)",
                project_memo::MAX_BYTES
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|_| ("invalid_request", "The memo is not UTF-8 text".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            // Made by this test for itself; it holds no user files.
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn store() -> (Dir, Store, String) {
        let dir = Dir(std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-memo-cli-{}", uuid::Uuid::new_v4())));
        let store = Store::open(dir.0.join("state")).unwrap();
        let root = dir.0.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let project = store.add_project(&root, Some("Notes")).unwrap();
        (dir, store, project.id)
    }

    fn run(store: &Store, args: &[&str], input: &str) -> Result<(Value, String, bool), Refusal> {
        result(
            store,
            args.iter().map(|a| (*a).to_owned()).collect(),
            &mut input.as_bytes(),
        )
    }

    #[test]
    fn show_and_set_speak_the_connectors_json() {
        let (_dir, store, id) = store();
        let (empty, _, _) = run(&store, &["show", &id], "").unwrap();
        assert_eq!(empty, json!({"v": 1, "project_id": id, "text": ""}));

        let (saved, _, was_set) = run(&store, &["set", "Notes"], "# Plan\r\n").unwrap();
        assert!(was_set);
        assert_eq!(saved["text"], "# Plan\n");
        assert!(saved["updated_at"].as_u64().unwrap() > 0);
        let (shown, text, _) = run(&store, &["show", &id], "").unwrap();
        assert_eq!(shown, saved);
        assert_eq!(text, "# Plan\n");
    }

    #[test]
    fn refusals_carry_codes() {
        let (_dir, store, id) = store();
        let code = |args: &[&str], input: &str| run(&store, args, input).unwrap_err().0;
        assert_eq!(code(&["show", "missing"], ""), "not_found");
        assert_eq!(code(&["set", "missing"], "x"), "not_found");
        assert_eq!(code(&["show"], ""), "invalid_request");
        assert_eq!(code(&["show", &id, "extra"], ""), "invalid_request");
        assert_eq!(code(&["get", &id], ""), "invalid_request");
        let over = "x".repeat(project_memo::MAX_BYTES + 1);
        assert_eq!(code(&["set", &id], &over), "invalid_request");
        let huge = "x".repeat(MAX_INPUT_BYTES + 1);
        assert_eq!(code(&["set", &id], &huge), "invalid_request");
        let mut not_utf8: &[u8] = &[0xff, 0xfe];
        assert_eq!(
            result(&store, vec!["set".into(), id.clone()], &mut not_utf8)
                .unwrap_err()
                .0,
            "invalid_request"
        );
        // Nothing refused was saved.
        assert!(store.snapshot().unwrap().project_memo(&id).is_empty());
    }
}
