use super::*;

fn header() -> Header {
    Header {
        from: "Codex chat \"Fix the build\" (1234abcd)".into(),
        model: Some("gpt-5".into()),
        project: Some("demo".into()),
        worktree: Some("main (/work/demo)".into()),
        directory: "/work/demo".into(),
        at: "2026-10-05T12:00:00Z".into(),
        note: Some("Use the Work account from here on.".into()),
        caveat: None,
    }
}

fn user(n: usize) -> Entry {
    Entry::User(format!("question number {n}"))
}

fn agent(n: usize) -> Entry {
    Entry::Agent(format!("answer number {n}"))
}

fn conversation(entries: Vec<Entry>) -> Body {
    Body::Conversation(entries)
}

/// The two numbers of "the newest N of M" in a document's heading.
fn newest_of(document: &str) -> Option<(usize, usize)> {
    let rest = document.split("the newest ").nth(1)?;
    let mut numbers = rest
        .split_whitespace()
        .filter_map(|word| word.parse::<usize>().ok());
    Some((numbers.next()?, numbers.next()?))
}

#[test]
fn the_heading_names_the_source_the_time_and_the_note() {
    let text = &render(&header(), &conversation(vec![user(1), agent(1)]), BUDGET);
    assert!(text.starts_with("# Handoff\n"), "{text}");
    for wanted in [
        "- **From:** Codex chat \"Fix the build\" (1234abcd)",
        "- **Model:** gpt-5",
        "- **Project:** demo, worktree main (/work/demo)",
        "- **Directory:** `/work/demo`",
        "- **Written:** 2026-10-05T12:00:00Z",
        "- **Contents:** the whole conversation",
        "**Note from the person who handed this over:** Use the Work account from here on.",
    ] {
        assert!(text.contains(wanted), "missing {wanted:?} in\n{text}");
    }
    // Without a model, a project or a note the heading says nothing of them.
    let mut bare = header();
    (bare.model, bare.project, bare.worktree, bare.note) = (None, None, None, None);
    let text = render(&bare, &conversation(vec![user(1)]), BUDGET);
    for absent in ["Model", "Project", "Worktree", "Note from"] {
        assert!(!text.contains(absent), "{absent} in\n{text}");
    }
}

#[test]
fn messages_are_whole_and_commands_and_tools_are_short() {
    let long_message = format!(
        "Start of a long answer.\n\n{}\n\nEnd of it.",
        "A line.\n".repeat(400)
    );
    let output: String = (1..=40).map(|n| format!("output line {n}\n")).collect();
    let body = conversation(vec![
        Entry::User("Please fix `src/lib.rs`.\n\n```rust\nfn main() {}\n```".into()),
        Entry::Agent(long_message.clone()),
        Entry::Command {
            command: "cargo test --lib\nsecond line of the script".into(),
            output,
            exit_code: Some(101),
        },
        Entry::Files(vec![
            FileEdit {
                path: "src/lib.rs".into(),
                change: "edited",
            },
            FileEdit {
                path: "src/new.rs".into(),
                change: "added",
            },
        ]),
        Entry::Tool {
            name: "docs/search".into(),
            detail: "borrow checker".into(),
            output: None,
            failed: false,
        },
        Entry::Tool {
            name: "cua-driver/list_windows".into(),
            detail: String::new(),
            output: Some("driver is not running".into()),
            failed: true,
        },
        Entry::Search("rust E0502".into()),
        Entry::Checklist {
            title: "Plan",
            items: vec![
                (Mark::Done, "reproduce".into()),
                (Mark::Active, "fix".into()),
                (Mark::Pending, "document".into()),
            ],
        },
        Entry::Note("The context was compacted here.".into()),
    ]);
    let text = render(&header(), &body, BUDGET);
    // Messages as they were, fences and all.
    assert!(text.contains("### User\n\nPlease fix `src/lib.rs`.\n\n```rust\nfn main() {}\n```\n"));
    assert!(text.contains(&format!("### Agent\n\n{}\n", long_message.trim())));
    // A command: its first line, its exit status, the ends of its output.
    assert!(
        text.contains("**Ran** `cargo test --lib …` (exit 101)"),
        "{text}"
    );
    for kept in [
        "output line 1\n",
        "output line 6\n",
        "output line 35\n",
        "output line 40\n",
    ] {
        assert!(text.contains(kept), "missing {kept:?}");
    }
    assert!(text.contains("… 28 lines left out …"), "{text}");
    for dropped in ["output line 7\n", "output line 20\n", "output line 34\n"] {
        assert!(!text.contains(dropped), "kept {dropped:?}");
    }
    // The files by name, tools by name, a failure with what it said.
    assert!(text.contains("**Changed files:** `src/lib.rs` (edited), `src/new.rs` (added)"));
    assert!(text.contains("**Tool** `docs/search` borrow checker\n"));
    assert!(text.contains(
        "**Tool** `cua-driver/list_windows` (failed)\n\n```text\ndriver is not running\n```"
    ));
    assert!(text.contains("**Searched the web:** rust E0502"));
    assert!(text.contains("- [x] reproduce\n- [~] fix\n- [ ] document\n"));
    assert!(text.contains("*The context was compacted here.*"));
}

#[test]
fn a_short_output_is_shown_whole_and_an_empty_one_not_at_all() {
    let body = conversation(vec![
        Entry::Command {
            command: "ls".into(),
            output: "a\nb\nc\n".into(),
            exit_code: Some(0),
        },
        Entry::Command {
            command: "true".into(),
            output: "\n  \n".into(),
            exit_code: Some(0),
        },
    ]);
    let text = render(&header(), &body, BUDGET);
    assert!(
        text.contains("**Ran** `ls`\n\n```text\na\nb\nc\n```"),
        "{text}"
    );
    assert!(text.contains("**Ran** `true`\n\n"), "{text}");
    assert!(!text.contains("(exit 0)"));
    assert_eq!(text.matches("```text").count(), 1, "{text}");
}

#[test]
fn backticks_in_output_and_in_commands_cannot_break_out() {
    let body = conversation(vec![Entry::Command {
        command: "echo `date`".into(),
        output: "a ``` fence ````` inside".into(),
        exit_code: None,
    }]);
    let text = render(&header(), &body, BUDGET);
    assert!(text.contains("**Ran** `` echo `date` ``"), "{text}");
    assert!(
        text.contains("``````text\na ``` fence ````` inside\n``````\n"),
        "{text}"
    );
}

#[test]
fn a_long_conversation_keeps_the_newest_items_and_says_what_it_left_out() {
    let filler = "x".repeat(1000);
    let entries: Vec<Entry> = (0..300)
        .flat_map(|n| {
            [
                Entry::User(format!("question {n:03} {filler}")),
                Entry::Agent(format!("answer {n:03} {filler}")),
            ]
        })
        .collect();
    let total = entries.len();
    let budget = 40 * 1024;
    let text = &render(&header(), &conversation(entries), budget);
    assert!(text.len() <= budget, "{} bytes", text.len());
    // The newest verbatim, the oldest gone.
    assert!(text.contains("answer 299 "));
    assert!(text.contains("question 299 "));
    assert!(!text.contains("answer 001 "));
    // The first request stays at the top, then the marker, then the newest part.
    let first = text
        .find("question 000 ")
        .expect("the first request is kept");
    let marker = text.find("earlier items (about").expect("a marker");
    let newest = text.find("answer 299 ").unwrap();
    assert!(
        first < marker && marker < newest,
        "{first} {marker} {newest}"
    );
    assert!(text.contains("The conversation began like this:"));
    // The heading says how many of how many are there.
    let (kept, of) = newest_of(text).expect("the heading counts the items");
    assert_eq!(of, total);
    assert!(kept < total && kept > 20, "{kept}");
    // What is kept is a suffix, with no gaps: every item from the cut to the end, and the
    // first request ahead of them.
    assert_eq!(text.matches("answer ").count(), (kept - 1) / 2);
    assert_eq!(text.matches("question ").count(), (kept - 1) / 2 + 1);
}

#[test]
fn the_real_budget_holds_for_a_conversation_of_megabytes() {
    let filler = "word ".repeat(2000);
    let entries: Vec<Entry> = (0..400)
        .map(|n| {
            if n % 2 == 0 {
                Entry::User(format!("{n} {filler}"))
            } else {
                Entry::Agent(format!("{n} {filler}"))
            }
        })
        .collect();
    let text = render(&header(), &conversation(entries), BUDGET);
    assert!(text.len() <= BUDGET, "{} bytes", text.len());
    assert!(text.contains("399 word"));
    assert!(text.contains("left out to keep this within 200 KB"));
}

#[test]
fn one_enormous_message_keeps_its_start_and_its_end() {
    let message = format!("BEGIN{}END", "m".repeat(600 * 1024));
    let text = &render(&header(), &conversation(vec![Entry::User(message)]), BUDGET);
    assert!(text.len() <= BUDGET, "{} bytes", text.len());
    assert!(text.contains("BEGINmmm") && text.contains("mmmEND"));
    assert!(text.contains("bytes of this message are left out"));
    // Cutting never splits a character.
    let multibyte = "é".repeat(100_000);
    let text = cut_middle(&multibyte, 10_001);
    assert!(text.len() < 10_300 && text.contains("left out"));
}

#[test]
fn a_scrollback_keeps_its_newest_lines_under_a_marker() {
    let scrollback: String = (1..=20_000)
        .map(|n| format!("scrollback line {n}\n"))
        .collect();
    let text = &render(&header(), &Body::Scrollback(scrollback), 30 * 1024);
    assert!(text.len() <= 30 * 1024, "{} bytes", text.len());
    assert!(text.contains("scrollback line 20000\n"));
    assert!(!text.contains("scrollback line 1\n"));
    assert!(text.contains("earlier lines are left out"));
    assert!(text.contains("screen text, not a structured conversation"));
    let (kept, of) = newest_of(text).expect("the heading counts the lines");
    assert_eq!(of, 20_000);
    assert!(kept > 500 && kept < 20_000, "{kept}");
    // A short one is whole, without the marker, its colors and cursor moves gone.
    let short = "\u{1b}[1mbold\u{1b}[0m text\r\nnext\u{1b}[2K line\n\n\n";
    let text = render(&header(), &Body::Scrollback(short.into()), BUDGET);
    assert!(
        text.contains("```text\nbold text\nnext line\n```"),
        "{text}"
    );
    assert!(!text.contains("left out"));
}

#[test]
fn a_summary_is_the_whole_document_after_the_heading() {
    let text = render(
        &header(),
        &Body::Summary("## Goal\nShip it.\n\n## Next\n- tests\n".into()),
        BUDGET,
    );
    assert!(text.contains("- **Contents:** a summary the agent wrote"));
    assert!(text.contains("## Summary\n"));
    assert!(text.contains("## Goal\nShip it.\n\n## Next\n- tests\n"));
}

#[test]
fn escape_sequences_are_dropped_and_text_kept() {
    assert_eq!(strip_escapes("\u{1b}[31mred\u{1b}[0m"), "red");
    assert_eq!(strip_escapes("a\u{1b}]0;title\u{7}b"), "ab");
    assert_eq!(
        strip_escapes("a\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
        "alink"
    );
    assert_eq!(strip_escapes("tab\tand\nline\r\n"), "tab\tand\nline\n");
    assert_eq!(strip_escapes("bell\u{7}"), "bell");
    assert_eq!(strip_escapes("é ✓ 日本"), "é ✓ 日本");
}

#[test]
fn summarising_a_line_never_runs_over_and_marks_the_cut() {
    assert_eq!(clip("short", 10), "short");
    assert_eq!(clip("abcdefghij", 5), "abcd…");
    assert_eq!(clip("日本語日本語", 4), "日本語…");
}
