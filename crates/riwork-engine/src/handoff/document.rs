//! The handoff document: a Markdown account of a conversation that a new agent reads to
//! take it over.
//!
//! Whatever the conversation came from (a chat's log, a Codex rollout, a Claude transcript,
//! a terminal's scrollback), the readers in `sources` turn it into `Entry`s and this module
//! writes them down. User and agent messages are kept whole; commands, file changes and
//! other tools are shortened to a line or two, since the agent can look at the files
//! themselves. When the whole is more than the budget, the newest items are kept as they
//! are and the oldest are left out under a marker that says so.

use std::path::PathBuf;

/// The most a document may hold, in bytes.
pub const BUDGET: usize = 200 * 1024;
/// The most one message may take of it; a longer one keeps its start and its end.
const MESSAGE_CAP: usize = BUDGET / 4;
/// The first request of a long conversation stays at the top when it is cut, up to this.
const FIRST_REQUEST_CAP: usize = 8 * 1024;
/// Room for the marker and the heading that a cut conversation adds.
const SLACK: usize = 1024;
/// The lines of a command's output that are kept at each end.
const OUTPUT_HEAD: usize = 6;
const OUTPUT_TAIL: usize = 6;
/// The longest line of a summary of a command, an output or a tool.
const LINE_CHARS: usize = 240;

/// One thing that happened in a conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    User(String),
    Agent(String),
    Command {
        command: String,
        output: String,
        exit_code: Option<i32>,
    },
    Files(Vec<FileEdit>),
    Tool {
        name: String,
        /// What it was asked, in a few words: a path, a pattern, a URL.
        detail: String,
        output: Option<String>,
        failed: bool,
    },
    Search(String),
    Checklist {
        title: &'static str,
        items: Vec<(Mark, String)>,
    },
    /// Something about the conversation itself: it was compacted here.
    Note(String),
    /// What the agent itself made of the conversation so far, when it compacted it.
    Summary(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Pending,
    Active,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEdit {
    pub path: String,
    /// "added", "edited", "deleted" or "renamed".
    pub change: &'static str,
}

/// What the document is about, for its heading.
#[derive(Clone, Debug)]
pub struct Header {
    /// The conversation's origin, such as `Codex chat "Fix the build" (1234abcd)`.
    pub from: String,
    pub model: Option<String>,
    /// The project's name and the worktree's branch, when RiWork knows them.
    pub project: Option<String>,
    pub worktree: Option<String>,
    pub directory: PathBuf,
    /// RFC 3339.
    pub at: String,
    pub note: Option<String>,
    /// Why this is less than the conversation, or other than it was asked for.
    pub caveat: Option<String>,
}

/// What follows the heading.
#[derive(Clone, Debug)]
pub enum Body {
    /// A conversation, item by item.
    Conversation(Vec<Entry>),
    /// A terminal's scrollback as text: no structure to go by.
    Scrollback(String),
    /// What the agent itself wrote when asked for a summary.
    Summary(String),
}

/// The document for `body` under `header`, at most `budget` bytes long (a little more
/// when the heading alone is longer). When items are left out, its heading says how many
/// of how many are there.
pub fn render(header: &Header, body: &Body, budget: usize) -> String {
    let head_budget = header_len(header);
    let room = budget.saturating_sub(head_budget + SLACK);
    let (text, contents) = match body {
        Body::Conversation(entries) => {
            let (text, kept) = conversation(entries, room);
            let contents = if entries.is_empty() {
                "a conversation with nothing in it yet".to_owned()
            } else if kept == entries.len() {
                "the whole conversation".to_owned()
            } else {
                format!(
                    "the newest {kept} of {} items; the oldest were left out to keep this within {} KB",
                    entries.len(),
                    budget / 1024
                )
            };
            (text, contents)
        }
        Body::Scrollback(scrollback) => {
            let (text, kept, total) = scrollback_section(scrollback, room);
            let contents = if kept == total {
                "the terminal's scrollback (screen text, not a structured conversation)".to_owned()
            } else {
                format!(
                    "the newest {kept} of {total} lines of the terminal's scrollback (screen text, not a structured conversation)"
                )
            };
            (text, contents)
        }
        Body::Summary(summary) => {
            let text = format!(
                "## Summary\n\nThe agent wrote this when asked to sum up where things stand.\n\n{}\n",
                cut_middle(summary.trim(), room.saturating_sub(200))
            );
            (
                text,
                "a summary the agent wrote of the conversation".to_owned(),
            )
        }
    };
    format!("{}{text}", header_text(header, &contents))
}

fn header_len(header: &Header) -> usize {
    header_text(header, "").len() + 160
}

fn header_text(header: &Header, contents: &str) -> String {
    let mut text = String::from("# Handoff\n\n");
    text.push_str("Written by RiWork (`riwork handoff`) so that a new agent can take over this conversation.\n\n");
    text.push_str(&format!("- **From:** {}\n", header.from));
    if let Some(model) = &header.model {
        text.push_str(&format!("- **Model:** {model}\n"));
    }
    match (&header.project, &header.worktree) {
        (Some(project), Some(worktree)) => {
            text.push_str(&format!("- **Project:** {project}, worktree {worktree}\n"))
        }
        (Some(project), None) => text.push_str(&format!("- **Project:** {project}\n")),
        (None, Some(worktree)) => text.push_str(&format!("- **Worktree:** {worktree}\n")),
        (None, None) => {}
    }
    text.push_str(&format!(
        "- **Directory:** {}\n",
        code_span(&header.directory.to_string_lossy())
    ));
    text.push_str(&format!("- **Written:** {}\n", header.at));
    if !contents.is_empty() {
        text.push_str(&format!("- **Contents:** {contents}\n"));
    }
    if let Some(caveat) = &header.caveat {
        text.push_str(&format!("- **Caveat:** {caveat}\n"));
    }
    if let Some(note) = &header.note {
        text.push_str(&format!(
            "\n**Note from the person who handed this over:** {note}\n"
        ));
    }
    text.push('\n');
    text
}

// ---- A conversation, item by item ----------------------------------------------------

/// The conversation as text within `room` bytes, and how many items it has. A cut keeps
/// the newest items, and the first request ahead of them if it is short enough.
fn conversation(entries: &[Entry], room: usize) -> (String, usize) {
    let blocks: Vec<String> = entries.iter().map(block).collect();
    let total: usize = blocks.iter().map(String::len).sum();
    let mut text = String::from("## Conversation\n\n");
    if total <= room {
        for block in &blocks {
            text.push_str(block);
        }
        return (text, entries.len());
    }
    // The newest items that fit, counting backwards.
    let first_request = entries
        .iter()
        .position(|entry| matches!(entry, Entry::User(_)))
        .map(|at| (at, cut_start(&blocks[at], FIRST_REQUEST_CAP)));
    let mut used = first_request.as_ref().map_or(0, |(_, block)| block.len());
    let mut first_kept = blocks.len();
    for (index, block) in blocks.iter().enumerate().rev() {
        if used + block.len() > room.saturating_sub(SLACK) {
            break;
        }
        used += block.len();
        first_kept = index;
    }
    let pinned = first_request.filter(|(at, _)| *at < first_kept);
    let left_out = first_kept - usize::from(pinned.is_some());
    if let Some((_, block)) = &pinned {
        text.push_str("The conversation began like this:\n\n");
        text.push_str(block);
    }
    let left_out_bytes: usize = blocks[..first_kept].iter().map(String::len).sum();
    text.push_str(&format!(
        "> {left_out} earlier items (about {} KB) are left out to keep this document short. What follows is the newest part, as it was.\n\n",
        left_out_bytes / 1024
    ));
    for block in &blocks[first_kept..] {
        text.push_str(block);
    }
    (
        text,
        entries.len() - first_kept + usize::from(pinned.is_some()),
    )
}

fn block(entry: &Entry) -> String {
    match entry {
        Entry::User(text) => format!("### User\n\n{}\n\n", cut_middle(text.trim(), MESSAGE_CAP)),
        Entry::Agent(text) => format!("### Agent\n\n{}\n\n", cut_middle(text.trim(), MESSAGE_CAP)),
        Entry::Command {
            command,
            output,
            exit_code,
        } => {
            let mut text = format!("**Ran** {}", code_span(&first_line(command)));
            if let Some(code) = exit_code.filter(|code| *code != 0) {
                text.push_str(&format!(" (exit {code})"));
            }
            text.push_str("\n\n");
            text.push_str(&excerpt(output));
            text
        }
        Entry::Files(files) => {
            let list: Vec<String> = files
                .iter()
                .map(|file| format!("{} ({})", code_span(&file.path), file.change))
                .collect();
            format!("**Changed files:** {}\n\n", list.join(", "))
        }
        Entry::Tool {
            name,
            detail,
            output,
            failed,
        } => {
            let mut text = format!("**Tool** {}", code_span(name));
            if !detail.is_empty() {
                text.push_str(&format!(" {}", clip(&one_line(detail), LINE_CHARS)));
            }
            if *failed {
                text.push_str(" (failed)");
            }
            text.push_str("\n\n");
            if let Some(output) = output {
                text.push_str(&excerpt(output));
            }
            text
        }
        Entry::Search(query) => format!(
            "**Searched the web:** {}\n\n",
            clip(&one_line(query), LINE_CHARS)
        ),
        Entry::Checklist { title, items } => {
            let mut text = format!("**{title}**\n\n");
            for (mark, item) in items {
                let box_ = match mark {
                    Mark::Pending => "[ ]",
                    Mark::Active => "[~]",
                    Mark::Done => "[x]",
                };
                text.push_str(&format!(
                    "- {box_} {}\n",
                    clip(&one_line(item), 2 * LINE_CHARS)
                ));
            }
            text.push('\n');
            text
        }
        Entry::Note(text) => format!("*{}*\n\n", clip(&one_line(text), 2 * LINE_CHARS)),
        Entry::Summary(text) => format!(
            "### Summary of the earlier conversation\n\n{}\n\n",
            cut_middle(text.trim(), MESSAGE_CAP)
        ),
    }
}

/// A command's or tool's output as a code block: all of a short one, the first and last
/// lines of a long one. Nothing for an output with nothing in it.
fn excerpt(output: &str) -> String {
    let clean = strip_escapes(output);
    let lines: Vec<&str> = clean.trim_end().lines().collect();
    if lines.iter().all(|line| line.trim().is_empty()) {
        return String::new();
    }
    let shown: Vec<String> = if lines.len() > OUTPUT_HEAD + OUTPUT_TAIL + 2 {
        let left_out = lines.len() - OUTPUT_HEAD - OUTPUT_TAIL;
        lines[..OUTPUT_HEAD]
            .iter()
            .map(|line| clip(line, LINE_CHARS))
            .chain([format!("… {left_out} lines left out …")])
            .chain(
                lines[lines.len() - OUTPUT_TAIL..]
                    .iter()
                    .map(|line| clip(line, LINE_CHARS)),
            )
            .collect()
    } else {
        lines.iter().map(|line| clip(line, LINE_CHARS)).collect()
    };
    let body = shown.join("\n");
    let fence = fence_for(&body);
    format!("{fence}text\n{body}\n{fence}\n\n")
}

// ---- A scrollback ----------------------------------------------------------------------

/// The newest lines of `scrollback` that fit `room`, as a code block, with the number of
/// lines kept and of lines there were.
fn scrollback_section(scrollback: &str, room: usize) -> (String, usize, usize) {
    let clean = strip_escapes(scrollback);
    let lines: Vec<&str> = clean.trim_end().lines().map(str::trim_end).collect();
    let total = lines.len();
    let mut used = 0;
    let mut first = total;
    for (index, line) in lines.iter().enumerate().rev() {
        let size = line.len().min(4 * LINE_CHARS) + 1;
        if used + size > room.saturating_sub(SLACK) {
            break;
        }
        used += size;
        first = index;
    }
    let body = lines[first..]
        .iter()
        .map(|line| clip(line, 4 * LINE_CHARS))
        .collect::<Vec<_>>()
        .join("\n");
    let fence = fence_for(&body);
    let mut text = String::from("## Terminal scrollback\n\n");
    if first > 0 {
        text.push_str(&format!(
            "> {first} earlier lines are left out to keep this document short. What follows is the newest part.\n\n"
        ));
    }
    text.push_str(&format!("{fence}text\n{body}\n{fence}\n"));
    (text, total - first, total)
}

// ---- Text helpers ----------------------------------------------------------------------

fn first_line(text: &str) -> String {
    let mut lines = text.trim().lines();
    let first = clip(lines.next().unwrap_or(""), LINE_CHARS);
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `max` characters of `text`, with `…` where it was cut.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Inline code that survives a backtick inside it.
fn code_span(text: &str) -> String {
    let longest = longest_run(text, '`');
    let ticks = "`".repeat(longest + 1);
    if text.starts_with('`') || text.ends_with('`') {
        format!("{ticks} {text} {ticks}")
    } else {
        format!("{ticks}{text}{ticks}")
    }
}

/// A fence for a code block that `text` cannot end early.
fn fence_for(text: &str) -> String {
    "`".repeat((longest_run(text, '`') + 1).max(3))
}

fn longest_run(text: &str, wanted: char) -> usize {
    let (mut longest, mut run) = (0, 0);
    for character in text.chars() {
        run = if character == wanted { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// `text` within `max` bytes: a longer one keeps its start and its end around a marker
/// that says how much was left out.
pub fn cut_middle(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let keep = max.saturating_sub(120);
    let (head, tail) = (keep * 6 / 10, keep * 4 / 10);
    let start = floor_boundary(text, head);
    let end = ceil_boundary(text, text.len() - tail);
    format!(
        "{}\n\n[… {} bytes of this message are left out …]\n\n{}",
        &text[..start],
        end - start,
        &text[end..]
    )
}

/// The first `max` bytes of `text`, as a block with a note where it was cut.
fn cut_start(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let end = floor_boundary(text, max.saturating_sub(80));
    format!(
        "{}\n\n[… the rest of this message is left out …]\n\n",
        &text[..end]
    )
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil_boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// `text` without terminal escape sequences and control characters (tabs and newlines
/// stay), as a command prints them and a terminal's capture holds them.
pub fn strip_escapes(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\u{1b}' => match characters.peek() {
                // CSI: parameters, then a final byte from `@` to `~`.
                Some('[') => {
                    characters.next();
                    for next in characters.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC: up to a bell or the string terminator.
                Some(']') => {
                    characters.next();
                    while let Some(next) = characters.next() {
                        if next == '\u{7}' || (next == '\u{1b}' && characters.peek() == Some(&'\\'))
                        {
                            if next == '\u{1b}' {
                                characters.next();
                            }
                            break;
                        }
                    }
                }
                Some(_) => {
                    characters.next();
                }
                None => {}
            },
            '\r' => {}
            '\n' | '\t' => clean.push(character),
            other if other.is_control() => {}
            other => clean.push(other),
        }
    }
    clean
}

#[cfg(test)]
mod tests;
