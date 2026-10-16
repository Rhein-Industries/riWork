//! Provider-independent presentation hints. Drivers attach these while their
//! discriminators are available; `enrich` is the deliberately weaker legacy path.
//! This module never changes bodies, stored output, item identities or sequence numbers.

use serde_json::Value;

use super::model::{Item, ItemBody, LineRange, Semantics, ToolKind, Worker};

/// Fill missing presentation metadata without overriding a provider's decision.
/// An unclassified tool stays absent: converting it to `Other` here would make
/// inferred metadata indistinguishable from an explicitly supplied `Other`
/// before the transcript can merge earlier provider-only lifecycle hints.
pub fn enrich(item: &mut Item) {
    if item.presentation.semantics.is_none() {
        item.presentation.semantics =
            classify_body(&item.body).filter(|semantics| semantics.kind != ToolKind::Other);
    }
}

/// Classify the incoming body before consulting an earlier lifecycle value.
/// Starts often have incomplete arguments, so their fallback must not mask the
/// completed body. An unclassifiable/generic body can inherit earlier hints;
/// supplied metadata keeps its kind and every populated fact. Missing facts of
/// the same kind are filled for both raw and already-enriched replay events.
pub fn enrich_inheriting(item: &mut Item, previous: Option<&Semantics>) {
    let supplied = item.presentation.semantics.take();
    let semantics = supplied.or_else(|| match classify_body(&item.body) {
        Some(semantics) if semantics.kind != ToolKind::Other => Some(semantics),
        _ => previous.cloned(),
    });
    item.presentation.semantics = semantics.map(|mut semantics| {
        // A final legacy body may omit arguments that were present at start.
        // Populated final arguments always replace their earlier values.
        if let Some(previous) = previous.filter(|old| old.kind == semantics.kind) {
            semantics.target = semantics.target.or_else(|| previous.target.clone());
            semantics.range = semantics.range.or_else(|| previous.range.clone());
            semantics.match_count = semantics.match_count.or(previous.match_count);
            semantics.excerpt = semantics.excerpt.or_else(|| previous.excerpt.clone());
            if semantics.produced.is_empty() {
                semantics.produced = previous.produced.clone();
            }
            semantics.worker = semantics.worker.or_else(|| previous.worker.clone());
        }
        semantics
    });
}

/// Fill output evidence after lifecycle metadata has been merged. Classification
/// remains absent-only: this never changes a provider's kind or supplied facts.
pub fn refresh_output(item: &mut Item) {
    if let (Some(semantics), ItemBody::ToolCall { output, .. }) =
        (&mut item.presentation.semantics, &item.body)
    {
        capture_output(semantics, output.as_deref());
    }
}

/// The information recoverable from a legacy item, with no provider guessing.
pub fn classify_body(body: &ItemBody) -> Option<Semantics> {
    match body {
        ItemBody::Command { command, .. } => Some(Semantics {
            kind: ToolKind::Shell,
            target: (!command.trim().is_empty()).then(|| command.clone()),
            ..Default::default()
        }),
        ItemBody::WebSearch { query } => Some(Semantics {
            kind: ToolKind::WebSearch,
            target: (!query.trim().is_empty()).then(|| query.clone()),
            ..Default::default()
        }),
        ItemBody::ToolCall {
            server,
            tool,
            input,
            output,
        } => Some(tool_semantics(
            tool,
            server.as_deref(),
            input,
            output.as_deref(),
        )),
        _ => None,
    }
}

fn argument(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| {
            input[*key]
                .as_str()
                .filter(|value| !value.trim().is_empty())
        })
        .map(str::to_owned)
}

/// Tool-name fallback shared with the provider converters. An unknown tool stays
/// `Other`; a role-named old Muse subagent cannot safely be reconstructed here.
pub fn tool_semantics(
    tool: &str,
    server: Option<&str>,
    input: &Value,
    output: Option<&str>,
) -> Semantics {
    let name = tool.to_ascii_lowercase().replace(['_', '-'], "");
    let kind = if server.is_some() || tool.starts_with("mcp__") {
        ToolKind::Mcp
    } else {
        match name.as_str() {
            "read" | "readfile" | "fileread" => ToolKind::Read,
            "grep" | "search" | "searchfiles" | "filesearch" | "ripgrep" => ToolKind::Search,
            "glob" | "ls" | "list" | "listfiles" | "listdirectory" => ToolKind::List,
            "websearch" => ToolKind::WebSearch,
            "webfetch" | "fetchurl" => ToolKind::WebFetch,
            "bash" | "shell" | "exec" | "execcommand" | "terminal" | "command" => ToolKind::Shell,
            "task" | "agent" | "subagent" | "collabagenttoolcall" => ToolKind::Subagent,
            _ => ToolKind::Other,
        }
    };
    let target = match kind {
        ToolKind::Read => argument(input, &["file_path", "path", "filename", "file"]),
        ToolKind::Search => argument(input, &["pattern", "query", "search"]),
        ToolKind::List => argument(input, &["pattern", "path", "directory"]),
        ToolKind::WebSearch => argument(input, &["query", "search_term"]),
        ToolKind::WebFetch => argument(input, &["url", "uri"]),
        ToolKind::Shell => argument(input, &["command", "cmd"]),
        ToolKind::Subagent => argument(input, &["description", "objective", "prompt"]),
        ToolKind::Mcp | ToolKind::Other => {
            argument(input, &["file_path", "path", "query", "url", "command"])
        }
    };
    let range = if kind == ToolKind::Read {
        let start = input["offset"]
            .as_u64()
            .or_else(|| input["start_line"].as_u64());
        let limit = input["limit"].as_u64();
        (start.is_some() || limit.is_some()).then(|| {
            let start = start.unwrap_or(1).max(1);
            LineRange {
                start,
                end: limit
                    .filter(|limit| *limit > 0)
                    .and_then(|limit| start.checked_add(limit - 1)),
            }
        })
    } else {
        None
    };
    let worker = (kind == ToolKind::Subagent).then(|| Worker {
        role: argument(input, &["subagent_type", "role", "agent_type"]),
        objective: argument(input, &["description", "objective", "prompt"]),
        child_count: input["child_count"].as_u64(),
    });
    let mut semantics = Semantics {
        kind,
        target,
        range,
        worker,
        ..Default::default()
    };
    capture_output(&mut semantics, output);
    semantics
}

/// Capture only evidence present in an output. No match count is inferred from
/// arbitrary prose (and an absent or elided output therefore stays unknown).
pub fn capture_output(semantics: &mut Semantics, output: Option<&str>) {
    let Some(output) = output.filter(|text| !text.trim().is_empty()) else {
        return;
    };
    if semantics.kind == ToolKind::Search && semantics.match_count.is_none() {
        let count = output
            .lines()
            .filter(|line| {
                let mut parts = line.split(':');
                parts.next().is_some_and(|path| !path.is_empty())
                    && parts
                        .next()
                        .is_some_and(|number| number.parse::<u64>().is_ok())
                    && parts.next().is_some()
            })
            .count();
        if count > 0 {
            semantics.match_count = Some(count as u64);
        }
    }
    if matches!(semantics.kind, ToolKind::Read | ToolKind::WebFetch) && semantics.excerpt.is_none()
    {
        // A compact hint only; complete output continues to live in ItemBody.
        semantics.excerpt = output
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| line.chars().take(240).collect());
    }
}

/// Conservative grouping predicate from transcript-grouping v2. Shell quoting is
/// parsed before checking every pipeline/conditional command. Redirection,
/// substitution, background jobs and execution/write flags are never exploratory.
/// This is a display classifier, not a command authorization mechanism.
pub fn is_read_only_shell(command: &str) -> bool {
    let Some(commands) = shell_commands(command) else {
        return false;
    };
    !commands.is_empty() && commands.iter().all(|words| read_only_words(words))
}

fn read_only_words(words: &[String]) -> bool {
    let Some(program) = words.first().map(String::as_str) else {
        return false;
    };
    // Absolute executables, wrappers and variable assignments are intentionally
    // outside the short first-word allow-list in the spec.
    let args = &words[1..];
    match program {
        "ls" | "cat" | "head" | "tail" => true,
        "rg" => !args.iter().any(|arg| {
            arg == "--pre"
                || arg.starts_with("--pre=")
                || arg == "--hostname-bin"
                || arg.starts_with("--hostname-bin=")
        }),
        "grep" => true,
        "find" => !args.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "-delete"
                    | "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-fprint"
                    | "-fprint0"
                    | "-fprintf"
                    | "-fls"
            )
        }),
        "git" => {
            args.first()
                .is_some_and(|arg| matches!(arg.as_str(), "status" | "log" | "diff" | "show"))
                && !args.iter().any(|arg| {
                    arg == "--output"
                        || arg.starts_with("--output=")
                        || arg == "--ext-diff"
                        || arg == "--textconv"
                })
        }
        "sed" => {
            let mut args = args.iter();
            args.next().is_some_and(|arg| arg == "-n")
                && args.next().is_some_and(|script| {
                    script.split(';').all(|part| {
                        part.trim().strip_suffix('p').is_some_and(|address| {
                            address.chars().all(|ch| {
                                ch.is_ascii_digit() || matches!(ch, ',' | '$' | ' ' | '\t')
                            })
                        })
                    })
                })
                && args.all(|arg| !arg.starts_with('-'))
        }
        _ => false,
    }
}

fn shell_commands(command: &str) -> Option<Vec<Vec<String>>> {
    let mut commands = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut word_started = false;
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if quote == Some('\'') {
            if ch == '\'' {
                quote = None;
            } else {
                word.push(ch);
            }
            continue;
        }
        if ch == '$' || ch == '`' {
            return None;
        }
        if ch == '\\' {
            word.push(chars.next()?);
            word_started = true;
            continue;
        }
        if quote == Some('"') {
            if ch == '"' {
                quote = None;
            } else {
                word.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                word_started = true;
            }
            '<' | '>' | '(' | ')' => return None,
            '|' | '&' | ';' | '\n' => {
                if word_started {
                    words.push(std::mem::take(&mut word));
                    word_started = false;
                }
                if words.is_empty() {
                    return None;
                }
                if ch == '&' && chars.next() != Some('&') {
                    return None;
                }
                if ch == '|' && chars.peek() == Some(&'|') {
                    chars.next();
                }
                commands.push(std::mem::take(&mut words));
            }
            ch if ch.is_whitespace() => {
                if word_started {
                    words.push(std::mem::take(&mut word));
                    word_started = false;
                }
            }
            _ => {
                word.push(ch);
                word_started = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if word_started {
        words.push(word);
    }
    if words.is_empty() {
        return None;
    }
    commands.push(words);
    Some(commands)
}
