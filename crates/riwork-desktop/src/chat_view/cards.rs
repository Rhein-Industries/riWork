//! What a transcript item looks like in the list, before any drawing: the one-line head of
//! each card, its status badge, and the text a card holds shortened and cleaned for the
//! screen.

use crate::chat::model::{
    ChangeKind, FileChange, Item, ItemBody, ItemStatus, NoticeLevel, Step, StepStatus, ToolKind,
};
use serde_json::Value;

/// How loudly a badge or a line speaks; the view picks the theme color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Muted,
    Accent,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub label: String,
    pub tone: Tone,
    /// Something is still going on, so the badge pulses.
    pub live: bool,
}

impl Badge {
    fn new(label: impl Into<String>, tone: Tone) -> Self {
        Self {
            label: label.into(),
            tone,
            live: false,
        }
    }
}

/// The first line of a card: what it is, a second muted piece of it, and a badge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub title: String,
    pub subtitle: Option<String>,
    pub badge: Option<Badge>,
    /// There is a body to show or hide.
    pub expandable: bool,
}

/// Lines added and removed, per file of a file change, in order.
pub type FileStats = Vec<(usize, usize)>;

/// A command's longest title; longer ones are cut with an ellipsis.
const TITLE_CHARS: usize = 200;

pub fn head(item: &Item, stats: Option<&FileStats>) -> Option<Head> {
    let status = item.status;
    Some(match &item.body {
        ItemBody::Command {
            command,
            cwd,
            output,
            exit_code,
        } => Head {
            title: first_line(command),
            subtitle: cwd.as_deref().map(short_path),
            badge: command_badge(status, *exit_code),
            expandable: !output.trim().is_empty() || command.trim().contains('\n'),
        },
        ItemBody::FileChange { changes } => Head {
            title: file_change_title(changes),
            subtitle: stats
                .map(|stats| change_counts(stats))
                .filter(|s| !s.is_empty()),
            badge: status_badge(status),
            expandable: changes.iter().any(|change| change.diff.is_some()) || changes.len() > 1,
        },
        ItemBody::ToolCall {
            server,
            tool,
            input,
            output,
        } => {
            let (server, tool) = tool_name(server.as_deref(), tool);
            Head {
                title: match server {
                    Some(server) => format!("{server} · {tool}"),
                    None => tool.clone(),
                },
                subtitle: Some(input_summary(input)).filter(|s| !s.is_empty()),
                badge: status_badge(status),
                expandable: !input.is_null() || output.as_ref().is_some_and(|o| !o.is_empty()),
            }
        }
        ItemBody::Plan { steps, .. } => Head {
            title: format!("Plan · {}", progress(steps)),
            // The explanation is the first line of the card's body.
            subtitle: None,
            badge: None,
            expandable: true,
        },
        ItemBody::Todo { items } => Head {
            title: format!("Todo · {}", progress(items)),
            subtitle: None,
            badge: None,
            expandable: true,
        },
        ItemBody::WebSearch { query } => Head {
            title: format!("Web search · {}", first_line(query)),
            subtitle: None,
            badge: status_badge(status),
            expandable: false,
        },
        ItemBody::Reasoning { text } => Head {
            title: if status == ItemStatus::InProgress {
                "Thinking…".to_owned()
            } else {
                "Thought".to_owned()
            },
            subtitle: None,
            badge: None,
            expandable: !text.trim().is_empty(),
        },
        ItemBody::UserMessage { .. }
        | ItemBody::AgentMessage { .. }
        | ItemBody::Compaction
        | ItemBody::Notice { .. } => return None,
    })
}

fn command_badge(status: ItemStatus, exit_code: Option<i32>) -> Option<Badge> {
    match (status, exit_code) {
        (ItemStatus::Completed, Some(0)) => None,
        (ItemStatus::Completed | ItemStatus::Failed, Some(code)) if code != 0 => {
            Some(Badge::new(format!("Exit {code}"), Tone::Error))
        }
        _ => status_badge(status),
    }
}

/// The badge a status has on its own. A finished item that went well has none.
pub fn status_badge(status: ItemStatus) -> Option<Badge> {
    match status {
        ItemStatus::InProgress => Some(Badge {
            label: "Running".into(),
            tone: Tone::Accent,
            live: true,
        }),
        ItemStatus::Completed => None,
        ItemStatus::Failed => Some(Badge::new("Failed", Tone::Error)),
        ItemStatus::Declined => Some(Badge::new("Declined", Tone::Warning)),
        ItemStatus::Interrupted => Some(Badge::new("Interrupted", Tone::Muted)),
    }
}

/// Header details stay factual: absent metadata never turns into an invented count.
pub fn metadata(item: &Item, head: Option<&Head>) -> Vec<String> {
    let mut parts = Vec::new();
    match &item.body {
        ItemBody::Command { cwd: Some(cwd), .. } => parts.push(short_path(cwd)),
        ItemBody::FileChange { .. } => {
            if let Some(counts) = head.and_then(|head| head.subtitle.as_ref()) {
                parts.push(counts.clone());
            }
        }
        _ => {}
    }
    if let Some(semantics) = &item.presentation.semantics {
        if let Some(range) = &semantics.range {
            parts.push(match range.end {
                Some(end) if end != range.start => format!("L{}–{end}", range.start),
                _ => format!("L{}", range.start),
            });
        }
        if let Some(count) = semantics.match_count {
            parts.push(format!(
                "{count} {}",
                if count == 1 { "match" } else { "matches" }
            ));
        }
        if semantics.kind == ToolKind::Subagent {
            if let Some(worker) = &semantics.worker {
                if let Some(role) = &worker.role {
                    parts.push(role.clone());
                }
                if let Some(count) = worker.child_count {
                    parts.push(format!("{count} tools"));
                }
            }
        }
    }
    parts
}

pub fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

/// The compact output is the first three and last twelve lines. A running row
/// deliberately exposes only a three-line live tail after manual disclosure.
#[derive(Debug, PartialEq, Eq)]
pub struct OutputPreview {
    pub text: String,
    pub omitted: usize,
    /// The synthetic head/tail separator, never part of a drag-copy payload.
    pub omission_line: Option<usize>,
}

impl OutputPreview {
    pub fn selectable_lines(&self) -> impl Iterator<Item = (usize, &str)> {
        self.text
            .split('\n')
            .enumerate()
            .filter(move |(line, _)| !self.text.is_empty() && Some(*line) != self.omission_line)
    }
}

/// Adjacent ordinary output lines share one text layout. Explicit errors retain
/// their full-width row tint, and the synthetic omission marker is not selectable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputRunKind {
    Plain,
    Error,
    Omission,
}

#[derive(Debug, PartialEq, Eq)]
pub struct OutputRun {
    pub first_line: usize,
    pub text: String,
    pub kind: OutputRunKind,
}

impl OutputRun {
    pub fn selection_key(&self, base: &str) -> Option<String> {
        match self.kind {
            OutputRunKind::Plain => Some(format!("{base}:run:{}", self.first_line)),
            OutputRunKind::Error => Some(format!("{base}:{}", self.first_line)),
            OutputRunKind::Omission => None,
        }
    }
}

pub fn output_runs(preview: &OutputPreview) -> Vec<OutputRun> {
    let mut runs: Vec<OutputRun> = Vec::new();
    if preview.text.is_empty() {
        return runs;
    }
    for (line, text) in preview.text.split('\n').enumerate() {
        let kind = if preview.omission_line == Some(line) {
            OutputRunKind::Omission
        } else if output_line_is_error(text) {
            OutputRunKind::Error
        } else {
            OutputRunKind::Plain
        };
        if kind == OutputRunKind::Plain
            && let Some(previous) = runs.last_mut().filter(|run| run.kind == kind)
        {
            previous.text.push('\n');
            previous.text.push_str(text);
        } else {
            runs.push(OutputRun {
                first_line: line,
                text: text.to_owned(),
                kind,
            });
        }
    }
    runs
}

fn output_line_is_error(line: &str) -> bool {
    let lower = line.trim_start().to_ascii_lowercase();
    ["error", "stderr", "fatal", "panic", "panicked"]
        .iter()
        .any(|prefix| {
            lower
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || !rest.starts_with(char::is_alphanumeric))
        })
}

pub fn omission_label(count: usize, file: bool) -> String {
    let plural = if count == 1 { "" } else { "s" };
    if file {
        format!("… {count} more line{plural}")
    } else {
        format!("… {count} line{plural} omitted")
    }
}

pub fn output_preview(raw: &str, running: bool, show_all: bool) -> OutputPreview {
    let clean = clean_output_ref(raw);
    let raw = clean.as_ref();
    const BYTES: usize = 32 * 1024;
    if running || show_all {
        let (shown, omitted) = tail(raw, if running { 3 } else { 200 }, BYTES);
        return OutputPreview {
            text: shown.to_owned(),
            omitted,
            omission_line: None,
        };
    }
    let total = raw.trim_end_matches('\n').lines().count();
    if total <= 15 {
        let (shown, omitted) = tail(raw, 15, BYTES);
        return OutputPreview {
            text: shown.to_owned(),
            omitted,
            omission_line: None,
        };
    }
    let first_end = raw
        .match_indices('\n')
        .nth(2)
        .map_or(raw.len(), |(at, _)| at);
    let mut end = first_end.min(BYTES / 2);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    let first = raw[..end].trim_end_matches('\n');
    let (last, _) = tail(raw, 12, BYTES / 2);
    let omitted = total.saturating_sub(first.lines().count() + last.lines().count());
    OutputPreview {
        text: format!("{first}\n{}\n{}", omission_label(omitted, false), last),
        omitted,
        omission_line: Some(first.split('\n').count()),
    }
}

/// A file excerpt keeps source order and derives its gutter only from recorded numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExcerptLine {
    pub number: Option<usize>,
    pub text: String,
}

pub fn numbered_excerpt(raw: &str, range_start: Option<usize>) -> Vec<ExcerptLine> {
    raw.trim_end_matches('\n')
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let trimmed = line.trim_start();
            let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
            let prefix = &trimmed[..digits];
            let rest = &trimmed[digits..];
            let numbered = (digits > 0)
                .then(|| prefix.parse::<usize>().ok())
                .flatten()
                .and_then(|number| {
                    rest.strip_prefix('→')
                        .or_else(|| rest.strip_prefix('\t'))
                        .map(|text| (number, text))
                });
            let (number, text) = match numbered {
                Some((number, text)) => (Some(number), text),
                None => (range_start.map(|start| start.saturating_add(index)), line),
            };
            ExcerptLine {
                number,
                text: clean_output(text),
            }
        })
        .collect()
}

/// Copy the complete stored excerpt, stripping provider gutter prefixes, never truncating.
pub fn excerpt_copy(raw: &str) -> String {
    let mut text = numbered_excerpt(raw, None)
        .into_iter()
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n");
    if raw.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Files read top-down: a bounded head, rather than a shell's head-and-tail preview.
pub fn file_output_preview(raw: &str, show_all: bool) -> OutputPreview {
    let text = excerpt_copy(raw);
    let lines: Vec<_> = text.trim_end_matches('\n').lines().collect();
    let cap = if show_all { 200 } else { 15 };
    let mut shown = String::new();
    let mut count = 0;
    for line in lines.iter().take(cap) {
        let extra = usize::from(count > 0);
        let available = (32 * 1024usize).saturating_sub(shown.len() + extra);
        if available == 0 {
            break;
        }
        if count > 0 {
            shown.push('\n');
        }
        let mut end = line.len().min(available);
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        shown.push_str(&line[..end]);
        count += 1;
        if end < line.len() {
            break;
        }
    }
    let partial = count > 0
        && lines.get(count - 1).is_some_and(|line| {
            shown
                .lines()
                .last()
                .is_some_and(|last| last.len() < line.len())
        });
    OutputPreview {
        text: shown,
        omitted: lines.len().saturating_sub(count) + usize::from(partial),
        omission_line: None,
    }
}

/// Terminal output has no stderr stream in stored history. Tint explicit error
/// lines rather than suggesting all lines of a failed command are errors.
pub fn error_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut offset = 0;
    text.split_inclusive('\n')
        .filter_map(|line| {
            let start = offset;
            offset += line.len();
            output_line_is_error(line).then_some(start..start + line.trim_end_matches('\n').len())
        })
        .collect()
}

/// The tone of a notice line.
pub fn notice_tone(level: NoticeLevel) -> Tone {
    match level {
        NoticeLevel::Info => Tone::Muted,
        NoticeLevel::Warning => Tone::Warning,
        NoticeLevel::Error => Tone::Error,
    }
}

/// Done steps out of all: `2/5 done`.
pub fn progress(steps: &[Step]) -> String {
    let done = steps
        .iter()
        .filter(|step| step.status == StepStatus::Completed)
        .count();
    format!("{done}/{} done", steps.len())
}

pub fn step_mark(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "○",
        StepStatus::InProgress => "◐",
        StepStatus::Completed => "✓",
    }
}

/// The letter on a file's badge and the word for its tooltip.
pub fn change_badge(kind: ChangeKind) -> (&'static str, &'static str) {
    match kind {
        ChangeKind::Add => ("A", "added"),
        ChangeKind::Modify => ("M", "modified"),
        ChangeKind::Delete => ("D", "deleted"),
        ChangeKind::Rename => ("R", "renamed"),
    }
}

fn file_change_title(changes: &[FileChange]) -> String {
    match changes {
        [] => "No file changes".to_owned(),
        [one] => format!("{} {}", verb(one.kind), one.path),
        many => {
            let kinds = many.iter().map(|change| change.kind).collect::<Vec<_>>();
            let verb = if kinds.iter().all(|kind| *kind == kinds[0]) {
                verb(kinds[0])
            } else {
                "Changed"
            };
            format!("{verb} {} files", many.len())
        }
    }
}

fn verb(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Add => "Added",
        ChangeKind::Modify => "Edited",
        ChangeKind::Delete => "Deleted",
        ChangeKind::Rename => "Renamed",
    }
}

/// `+12 −3` for the lines of all files together.
fn change_counts(stats: &[(usize, usize)]) -> String {
    let (added, removed) = stats
        .iter()
        .fold((0, 0), |(a, r), (added, removed)| (a + added, r + removed));
    match (added, removed) {
        (0, 0) => String::new(),
        (added, 0) => format!("+{added}"),
        (0, removed) => format!("−{removed}"),
        (added, removed) => format!("+{added} −{removed}"),
    }
}

/// The server and the tool to show. Claude names a tool of an MCP server
/// `mcp__server__tool`.
fn tool_name(server: Option<&str>, tool: &str) -> (Option<String>, String) {
    if let Some(rest) = tool.strip_prefix("mcp__")
        && let Some((server, tool)) = rest.split_once("__")
    {
        return (Some(server.to_owned()), tool.to_owned());
    }
    (server.map(str::to_owned), tool.to_owned())
}

/// The first known semantic argument, when present. Unknown object fields stay
/// absent from collapsed headers; their JSON is available only after disclosure.
pub fn input_summary(input: &Value) -> String {
    const USUAL: [&str; 11] = [
        "command",
        "file_path",
        "path",
        "pattern",
        "query",
        "url",
        "description",
        "prompt",
        "text",
        "name",
        "id",
    ];
    let summary = match input {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Object(fields) => USUAL
            .iter()
            .find_map(|key| fields.get(*key).and_then(Value::as_str))
            .map(str::to_owned)
            .unwrap_or_default(),
        _ => String::new(),
    };
    let line = first_line(&summary);
    if summary.trim().lines().count() > 1 {
        format!("{line} …")
    } else {
        line
    }
}

/// A tool's input as indented JSON, for the expanded card.
pub fn input_pretty(input: &Value) -> String {
    match input {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// The first line of `text`, trimmed and cut to a title's length.
pub fn first_line(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default().trim();
    if line.chars().count() > TITLE_CHARS {
        let cut = line.chars().take(TITLE_CHARS).collect::<String>();
        format!("{cut}…")
    } else {
        line.to_owned()
    }
}

/// A folder's last two parts: `~/work/app` is `work/app`.
fn short_path(path: &str) -> String {
    let parts = path
        .trim_end_matches('/')
        .rsplit('/')
        .take(2)
        .collect::<Vec<_>>();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

/// Command output as it is drawn: terminal escapes gone, a carriage return back to the
/// start of its line as a terminal shows it (so a progress bar is its last state), tabs
/// spread.
/// Ordinary output already has display-safe bytes. Borrow it until the bounded
/// preview is built rather than allocating and cleaning every historical line.
fn clean_output_ref(raw: &str) -> std::borrow::Cow<'_, str> {
    // C1 controls use the UTF-8 C2 prefix. Treat every C2 sequence as requiring
    // cleanup so printable Unicode still follows exactly the existing filter.
    if raw
        .as_bytes()
        .iter()
        .any(|&byte| byte < b' ' && byte != b'\n' || byte == 0x7f || byte == 0xc2)
    {
        std::borrow::Cow::Owned(clean_output(raw))
    } else {
        std::borrow::Cow::Borrowed(raw)
    }
}

pub fn clean_output(raw: &str) -> String {
    let mut lines = Vec::new();
    for line in raw.split('\n') {
        let shown = line
            .rsplit('\r')
            .find(|part| !part.is_empty())
            .unwrap_or("");
        lines.push(without_sgr(&crate::sgr::keep_sgr_only(shown)).replace('\t', "    "));
    }
    lines.join("\n")
}

/// `text` without the `ESC [ … m` sequences `sgr::keep_sgr_only` leaves in.
fn without_sgr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            for skipped in chars.by_ref() {
                if skipped == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The last lines of `text`, at most `max_lines` and `max_bytes`, and how many lines before
/// them were left out. Output is read from its end: that is where a build says why it
/// failed.
pub fn tail(text: &str, max_lines: usize, max_bytes: usize) -> (&str, usize) {
    let text = text.trim_end_matches('\n');
    let line_start = |before: usize| text[..before].rfind('\n').map_or(0, |at| at + 1);
    let mut start = line_start(text.len());
    let mut lines = 1;
    while start > 0 && lines < max_lines {
        let previous = line_start(start - 1);
        if text.len() - previous > max_bytes {
            break;
        }
        start = previous;
        lines += 1;
    }
    // A last line that is longer than the limit alone is cut from its front.
    let mut cut_line = 0;
    if text.len() - start > max_bytes {
        start = text.len() - max_bytes;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        cut_line = 1;
    }
    (
        &text[start..],
        text[..start].matches('\n').count() + cut_line,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_output_is_borrowed_and_control_cleanup_stays_exact() {
        let plain = "ordinary line\nUnicode résumé 東京\n";
        assert!(matches!(
            clean_output_ref(plain),
            std::borrow::Cow::Borrowed(_)
        ));
        for control in (0u8..=0x1f).chain(0x7f..=0x9f) {
            let source = format!("before{}after\n", char::from(control));
            assert_eq!(
                clean_output_ref(&source),
                clean_output(&source),
                "control {control:#x}"
            );
        }
        assert_eq!(clean_output_ref("left\tright\rrewritten"), "rewritten");
    }

    #[test]
    fn output_loses_escapes_and_keeps_the_last_state_of_a_progress_line() {
        assert_eq!(
            clean_output("\u{1b}[1;32m   Compiling\u{1b}[0m riwork\n"),
            "   Compiling riwork\n"
        );
        assert_eq!(clean_output("10%\r50%\r100%\ndone"), "100%\ndone");
        assert_eq!(clean_output("a\r\nb"), "a\nb");
        assert_eq!(clean_output("a\tb"), "a    b");
        assert_eq!(clean_output("\u{1b}]0;title\u{7}text"), "text");
    }

    #[test]
    fn a_long_output_is_read_from_its_end() {
        let lines: String = (1..=500).map(|n| format!("line {n}\n")).collect();
        let (shown, hidden) = tail(&lines, 200, 1 << 20);
        assert_eq!(shown.lines().count(), 200);
        assert!(shown.starts_with("line 301\n") && shown.ends_with("line 500"));
        assert_eq!(hidden, 300);
        // Short output is whole.
        assert_eq!(tail("a\nb\n", 200, 1 << 20), ("a\nb", 0));
        // The byte limit cuts lines too, and a single long line from its front.
        let (shown, hidden) = tail(&lines, 1000, 100);
        assert!(
            shown.len() <= 100 && hidden > 400,
            "{} {hidden}",
            shown.len()
        );
        let accents = "é".repeat(300);
        let (shown, hidden) = tail(&accents, 10, 100);
        assert!(shown.len() <= 100 && shown.chars().all(|c| c == 'é'));
        assert_eq!(hidden, 1);
        assert_eq!(tail("", 10, 10), ("", 0));
    }
    #[test]
    fn opened_output_shows_three_start_and_twelve_end_lines() {
        let output = (1..=40).map(|n| format!("line {n}\n")).collect::<String>();
        let preview = output_preview(&output, false, false);
        assert_eq!(preview.omitted, 25);
        assert!(preview.text.starts_with("line 1\nline 2\nline 3\n"));
        assert!(preview.text.contains("25 lines omitted\nline 29\n"));
        assert!(preview.text.ends_with("line 40"));
        assert!(!preview.text.contains("line 4\n"));
    }

    #[test]
    fn live_output_is_three_lines_and_show_all_stays_bounded() {
        let output = (1..=500).map(|n| format!("line {n}\n")).collect::<String>();
        let live = output_preview(&output, true, true);
        assert_eq!(live.text, "line 498\nline 499\nline 500");
        let all = output_preview(&output, false, true);
        assert_eq!(all.text.lines().count(), 200);
        assert_eq!(all.omitted, 300);
        let enormous = "é".repeat(100_000);
        let all = output_preview(&enormous, false, true);
        assert!(all.text.len() <= 32 * 1024);
        let tabs = output_preview(&"\t".repeat(100_000), false, true);
        assert!(tabs.text.len() <= 32 * 1024);
    }

    #[test]
    fn error_highlights_do_not_tint_ordinary_failed_command_output() {
        let text = "Checking code\n  error[E001]: bad type\nstderr: missing\nerrorless output\nfatal: exit\n";
        let ranges = error_ranges(text);
        assert_eq!(
            ranges
                .iter()
                .map(|range| &text[range.clone()])
                .collect::<Vec<_>>(),
            ["  error[E001]: bad type", "stderr: missing", "fatal: exit",]
        );
    }

    #[test]
    fn collapsed_unknown_input_never_falls_back_to_json() {
        assert!(input_summary(&serde_json::json!({"arbitrary": {"secret": "value"}})).is_empty());
    }
}

/// Provider placeholders add nothing when the actual image is already visible.
/// Keep captions and their whitespace intact, dropping only dedicated marker lines.
pub fn inline_image_output(output: &str, has_images: bool) -> std::borrow::Cow<'_, str> {
    if !has_images
        || !output
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case("[image]"))
    {
        return std::borrow::Cow::Borrowed(output);
    }
    std::borrow::Cow::Owned(
        output
            .lines()
            .filter(|line| !line.trim().eq_ignore_ascii_case("[image]"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}
