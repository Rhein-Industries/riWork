//! Direct typing: literal text and named keys delivered to a pane as one ordered
//! batch, under the same per-shell input lock as `submit`.
//!
//! Kept free of GPUI so `remote/tests` compiles it under strict Clippy against a
//! real tmux. `remote/src/rpc.rs` enforces the same limits on the wire; the
//! matrices in both crates' tests must stay identical.
use std::time::Duration;
use uuid::Uuid;

/// Items in one batch.
pub const MAX_ITEMS: usize = 64;
/// Text bytes across all items in one batch.
pub const MAX_TEXT_BYTES: usize = 4096;

/// A failure before any keystroke was delivered starts with one of these
/// tokens, so the connector can tell "nothing was typed, retry is safe" from
/// "some of it may have been". An error with none of them is uncertain.
pub const NOT_FOUND: &str = "not_found: ";
pub const INPUT_UNAVAILABLE: &str = "input_unavailable: ";
pub const NOT_SENT: &str = "not_sent: ";
pub const INVALID_REQUEST: &str = "invalid_request: ";

pub type Tmux<'a> = crate::session_viewport::Tmux<'a>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Enter,
    Tab,
    BTab,
    Escape,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// Control plus a lowercase ASCII letter.
    Ctrl(u8),
}

impl Key {
    /// The protocol name: `Enter`, `PageUp`, `C-c`, ...
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "Enter" => Self::Enter,
            "Tab" => Self::Tab,
            "BTab" => Self::BTab,
            "Escape" => Self::Escape,
            "Backspace" => Self::Backspace,
            "Delete" => Self::Delete,
            "Up" => Self::Up,
            "Down" => Self::Down,
            "Left" => Self::Left,
            "Right" => Self::Right,
            "Home" => Self::Home,
            "End" => Self::End,
            "PageUp" => Self::PageUp,
            "PageDown" => Self::PageDown,
            _ => match name.as_bytes() {
                [b'C', b'-', letter @ b'a'..=b'z'] => Self::Ctrl(*letter),
                _ => return None,
            },
        })
    }

    /// The name tmux's `send-keys` knows this key by.
    pub fn tmux_name(self) -> String {
        match self {
            Self::Enter => "Enter".into(),
            Self::Tab => "Tab".into(),
            Self::BTab => "BTab".into(),
            Self::Escape => "Escape".into(),
            Self::Backspace => "BSpace".into(),
            Self::Delete => "DC".into(),
            Self::Up => "Up".into(),
            Self::Down => "Down".into(),
            Self::Left => "Left".into(),
            Self::Right => "Right".into(),
            Self::Home => "Home".into(),
            Self::End => "End".into(),
            Self::PageUp => "PPage".into(),
            Self::PageDown => "NPage".into(),
            Self::Ctrl(letter) => format!("C-{}", char::from(letter)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Text(String),
    Key(Key),
}

/// Literal text: 1..=4096 bytes, no control characters, no line or paragraph
/// separators. Enter and Tab are keys, never text.
pub fn validate_text(text: &str) -> Result<(), String> {
    if text.is_empty() || text.len() > MAX_TEXT_BYTES {
        return Err(format!("text must be 1..={MAX_TEXT_BYTES} bytes"));
    }
    if text
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        return Err("text must not contain control characters; send keys for Enter or Tab".into());
    }
    Ok(())
}

/// One argv item: `t:<text>` or `k:<Name>`.
pub fn parse_item(argument: &str) -> Result<Item, String> {
    if let Some(text) = argument.strip_prefix("t:") {
        validate_text(text)?;
        Ok(Item::Text(text.to_owned()))
    } else if let Some(name) = argument.strip_prefix("k:") {
        Key::parse(name)
            .map(Item::Key)
            .ok_or_else(|| format!("unknown key '{name}'"))
    } else {
        Err("an item must be t:TEXT or k:KEY".into())
    }
}

/// The whole batch: 1..=64 items and at most 4096 text bytes in total.
pub fn validate(items: &[Item]) -> Result<(), String> {
    if items.is_empty() || items.len() > MAX_ITEMS {
        return Err(format!("a batch takes 1..={MAX_ITEMS} items"));
    }
    let mut total = 0usize;
    for item in items {
        if let Item::Text(text) = item {
            validate_text(text)?;
            total += text.len();
        }
    }
    if total > MAX_TEXT_BYTES {
        return Err(format!(
            "a batch takes at most {MAX_TEXT_BYTES} text bytes in total"
        ));
    }
    Ok(())
}

/// Parse argv items and validate the batch.
pub fn parse_items(arguments: &[String]) -> Result<Vec<Item>, String> {
    // Reject an oversized list before parsing every element of it.
    if arguments.len() > MAX_ITEMS {
        return Err(format!("a batch takes 1..={MAX_ITEMS} items"));
    }
    let items = arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| {
            parse_item(argument).map_err(|e| format!("item {}: {e}", index + 1))
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate(&items)?;
    Ok(items)
}

/// tmux reads an argument that ends in `;` as the end of a command, and one
/// that ends in `\;` as a literal `;`. Every argument sent through
/// `Command::args` therefore has its trailing `;` written as `\;`.
pub fn tmux_argument(argument: &str) -> String {
    match argument.strip_suffix(';') {
        Some(rest) => format!("{rest}\\;"),
        None => argument.to_owned(),
    }
}

/// One tmux invocation for these items: `send-keys` commands joined by `;`.
/// Runs of keys share a command; each text is its own `-l` command, and its
/// argument is escaped so a trailing `;` or `\;` arrives verbatim.
pub fn tmux_invocation(pane: &str, items: &[Item]) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut keys_open = false;
    for item in items {
        match item {
            Item::Key(key) => {
                if !keys_open {
                    if !args.is_empty() {
                        args.push(";".into());
                    }
                    args.extend(["send-keys".into(), "-t".into(), pane.to_owned()]);
                    keys_open = true;
                }
                args.push(key.tmux_name());
            }
            Item::Text(text) => {
                if !args.is_empty() {
                    args.push(";".into());
                }
                args.extend([
                    "send-keys".into(),
                    "-t".into(),
                    pane.to_owned(),
                    "-l".into(),
                    "--".into(),
                    tmux_argument(text),
                ]);
                keys_open = false;
            }
        }
    }
    args
}

/// How long a key waits after text that directly precedes it in a batch.
/// Codex reads characters arriving within 120 ms of each other as a paste and
/// then takes an Enter as a newline instead of a submit (see the settling
/// interval in `session_input`); a person typing never sends both that fast.
pub const KEY_AFTER_TEXT_PAUSE: Duration = Duration::from_millis(150);

/// The tmux invocations that deliver a batch, in order, with
/// `KEY_AFTER_TEXT_PAUSE` between consecutive ones. A new invocation starts
/// exactly where a key directly follows text: `[text, text]`, `[key, key]`
/// and `[key, text]` stay in one invocation, and a batch with no text-then-key
/// boundary is a single invocation.
pub fn tmux_plan(pane: &str, items: &[Item]) -> Vec<Vec<String>> {
    let mut plan = Vec::new();
    let mut start = 0;
    for (index, pair) in items.windows(2).enumerate() {
        if matches!(pair, [Item::Text(_), Item::Key(_)]) {
            plan.push(tmux_invocation(pane, &items[start..=index]));
            start = index + 1;
        }
    }
    if start < items.len() {
        plan.push(tmux_invocation(pane, &items[start..]));
    }
    plan
}

/// Deliver a batch to the shell's pane. Holds the shell's input lock for the
/// whole batch, pauses included, so nothing interleaves. Errors that begin
/// with a token above happened before any keystroke was sent.
pub fn send(home: &std::path::Path, id: &str, items: &[Item], t: &Tmux<'_>) -> Result<(), String> {
    validate(items).map_err(|e| format!("{INVALID_REQUEST}{e}"))?;
    if !Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
        return Err(format!("{INVALID_REQUEST}invalid UUID: {id}"));
    }
    let _lock =
        crate::session_viewport::lock(home, id, "input").map_err(|e| format!("{NOT_SENT}{e}"))?;
    let pane = format!("{id}:0.0");
    let status = t(&[
        "display-message",
        "-p",
        "-t",
        &pane,
        "#{pane_in_mode}|#{pane_input_off}",
    ])
    .map_err(|e| format!("{NOT_SENT}{e}"))?;
    let (in_mode, input_off) = status.trim().split_once('|').unwrap_or(("", ""));
    let flag = |value: &str| matches!(value, "0" | "1");
    if !flag(in_mode) || !flag(input_off) {
        return Err(format!("{NOT_SENT}tmux reported an unreadable pane state"));
    }
    if input_off == "1" {
        return Err(format!(
            "{INPUT_UNAVAILABLE}terminal input is disabled for this pane"
        ));
    }
    if in_mode == "1" {
        // Keystrokes in copy mode drive the viewer, not the program: leave it.
        if let Err(error) = t(&["send-keys", "-t", &pane, "-X", "cancel"]) {
            // The mode may have ended on its own since the check.
            let now = t(&["display-message", "-p", "-t", &pane, "#{pane_in_mode}"])
                .map_err(|e| format!("{NOT_SENT}{e}"))?;
            if now.trim() != "0" {
                return Err(format!("{NOT_SENT}cannot leave copy mode: {error}"));
            }
        }
    }
    // From here on tmux may have delivered part of the batch, so a failure
    // carries no token: the caller must treat the outcome as unknown.
    for (index, invocation) in tmux_plan(&pane, items).iter().enumerate() {
        if index > 0 {
            std::thread::sleep(KEY_AFTER_TEXT_PAUSE);
        }
        let borrowed: Vec<&str> = invocation.iter().map(String::as_str).collect();
        t(&borrowed)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const SHELL: &str = "00000000-0000-4000-8000-0000000000aa";

    fn text(value: &str) -> Item {
        Item::Text(value.to_owned())
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn every_protocol_key_name_maps_to_its_tmux_name() {
        let named = [
            ("Enter", "Enter"),
            ("Tab", "Tab"),
            ("BTab", "BTab"),
            ("Escape", "Escape"),
            ("Backspace", "BSpace"),
            ("Delete", "DC"),
            ("Up", "Up"),
            ("Down", "Down"),
            ("Left", "Left"),
            ("Right", "Right"),
            ("Home", "Home"),
            ("End", "End"),
            ("PageUp", "PPage"),
            ("PageDown", "NPage"),
        ];
        for (name, tmux) in named {
            assert_eq!(Key::parse(name).unwrap().tmux_name(), tmux, "{name}");
        }
        for letter in 'a'..='z' {
            let name = format!("C-{letter}");
            assert_eq!(Key::parse(&name).unwrap().tmux_name(), name);
        }
    }

    #[test]
    fn bad_key_names_are_rejected() {
        for name in [
            "", "enter", "ENTER", "Return", "Space", "F1", "PgUp", "PageUp ", " Enter", "C-",
            "C-A", "c-a", "C-1", "C-aa", "C--", "C-é", "M-a", "S-Tab", "BSpace", "DC", "PPage",
            "NPage", "Enter\n", "C-a\0",
        ] {
            assert_eq!(Key::parse(name), None, "{name:?}");
        }
    }

    #[test]
    fn text_rules() {
        for good in [
            "a",
            "hello world",
            " ",
            "  leading and trailing  ",
            "select 1;",
            "\\;",
            ";",
            "héllo ✓ 日本語 🚀",
            "-b -t #{pane_id} $(x) `y` '\"",
            &"x".repeat(MAX_TEXT_BYTES),
            "\u{a0}\u{200b}\u{feff}",
        ] {
            assert_eq!(validate_text(good), Ok(()), "{good:?}");
        }
        for bad in [
            "",
            "\n",
            "\r",
            "\t",
            "\0",
            "\u{1b}",
            "\u{7f}",
            "\u{85}",
            "\u{9f}",
            "a\u{2028}b",
            "a\u{2029}b",
            "line one\nline two",
            &"x".repeat(MAX_TEXT_BYTES + 1),
            &"é".repeat(MAX_TEXT_BYTES / 2 + 1),
        ] {
            assert!(validate_text(bad).is_err(), "{bad:?}");
        }
        // The limit counts bytes, not characters.
        assert_eq!(validate_text(&"é".repeat(MAX_TEXT_BYTES / 2)), Ok(()));
    }

    #[test]
    fn argv_encoding() {
        assert_eq!(parse_item("t:ls -la;"), Ok(text("ls -la;")));
        assert_eq!(parse_item("t: "), Ok(text(" ")));
        assert_eq!(parse_item("t:t:x"), Ok(text("t:x")));
        assert_eq!(parse_item("k:Enter"), Ok(Item::Key(Key::Enter)));
        assert_eq!(parse_item("k:C-c"), Ok(Item::Key(Key::Ctrl(b'c'))));
        for bad in [
            "", "t:", "k:", "Enter", "text", "T:x", "K:Enter", "k:enter", "k:F1", "k: Enter",
            "t:\n", "t:a\tb", "-x", "--json", ":x",
        ] {
            assert!(parse_item(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn batch_limits() {
        let one = strings(&["k:Enter"]);
        assert_eq!(parse_items(&one), Ok(vec![Item::Key(Key::Enter)]));
        assert!(parse_items(&[]).is_err());
        let sixty_four = vec!["k:Up".to_owned(); MAX_ITEMS];
        assert_eq!(parse_items(&sixty_four).unwrap().len(), MAX_ITEMS);
        let sixty_five = vec!["k:Up".to_owned(); MAX_ITEMS + 1];
        assert!(parse_items(&sixty_five).is_err());
        // 4096 bytes in total across items is the limit, not per item.
        let full = vec![format!("t:{}", "a".repeat(2048)); 2];
        assert!(parse_items(&full).is_ok());
        let over = strings(&[
            &format!("t:{}", "a".repeat(2048)),
            &format!("t:{}", "a".repeat(2049)),
        ]);
        assert!(parse_items(&over).unwrap_err().contains("text bytes"));
        // The error names the offending item.
        let error = parse_items(&strings(&["k:Enter", "k:Nope"])).unwrap_err();
        assert!(error.starts_with("item 2:"), "{error}");
    }

    #[test]
    fn consecutive_keys_share_one_send_keys_and_texts_stay_apart() {
        let items = [
            Item::Key(Key::Up),
            Item::Key(Key::Ctrl(b'c')),
            text("ls;"),
            text("a"),
            Item::Key(Key::Enter),
            Item::Key(Key::PageDown),
        ];
        assert_eq!(
            tmux_invocation("P", &items),
            strings(&[
                "send-keys",
                "-t",
                "P",
                "Up",
                "C-c",
                ";",
                "send-keys",
                "-t",
                "P",
                "-l",
                "--",
                "ls\\;",
                ";",
                "send-keys",
                "-t",
                "P",
                "-l",
                "--",
                "a",
                ";",
                "send-keys",
                "-t",
                "P",
                "Enter",
                "NPage"
            ])
        );
    }

    #[test]
    fn trailing_semicolons_are_escaped_for_tmux() {
        for (text, sent) in [
            ("a;", "a\\;"),
            (";", "\\;"),
            (";;", ";\\;"),
            ("\\;", "\\\\;"),
            ("a\\", "a\\"),
            ("a;b", "a;b"),
        ] {
            assert_eq!(tmux_argument(text), sent, "{text}");
        }
    }

    #[test]
    fn the_largest_batch_fits_one_tmux_client_message() {
        // A tmux client sends its whole argv in one message of under 16 KiB.
        let mut items: Vec<Item> = (0..MAX_ITEMS - 1)
            .map(|_| text(&"é".repeat(MAX_TEXT_BYTES / 2 / (MAX_ITEMS - 1))))
            .collect();
        items.push(text("é"));
        let pane = format!("{SHELL}:0.0");
        let bytes: usize = tmux_invocation(&pane, &items)
            .iter()
            .map(|a| a.len() + 1)
            .sum();
        assert!(bytes < 12 * 1024, "{bytes}");
    }

    /// Records every tmux call. Successive `display-message` calls answer with
    /// successive `statuses` (the last one repeats); `fail` names a subcommand
    /// that errors, and `fail_cancel` makes `send-keys -X` fail.
    struct Recorder {
        calls: RefCell<Vec<Vec<String>>>,
        times: RefCell<Vec<std::time::Instant>>,
        statuses: Vec<&'static str>,
        fail: Option<&'static str>,
        fail_cancel: bool,
    }

    impl Recorder {
        fn new(status: &'static str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                times: RefCell::new(Vec::new()),
                statuses: vec![status],
                fail: None,
                fail_cancel: false,
            }
        }

        /// The time between each call and the one before it.
        fn gaps(&self) -> Vec<Duration> {
            self.times
                .borrow()
                .windows(2)
                .map(|pair| pair[1] - pair[0])
                .collect()
        }

        fn run(&self, args: &[&str]) -> Result<String, String> {
            self.times.borrow_mut().push(std::time::Instant::now());
            let mut calls = self.calls.borrow_mut();
            calls.push(args.iter().map(|a| (*a).to_owned()).collect());
            let asked = calls
                .iter()
                .filter(|call| call[0] == "display-message")
                .count();
            if self.fail == Some(args[0]) || (self.fail_cancel && args.contains(&"-X")) {
                return Err(format!("{} failed", args[0]));
            }
            Ok(if args[0] == "display-message" {
                self.statuses[(asked - 1).min(self.statuses.len() - 1)].to_owned()
            } else {
                String::new()
            })
        }

        fn send(&self, items: &[Item]) -> Result<(), String> {
            let home = std::env::temp_dir().join(format!("riwork-keys-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&home).unwrap();
            let result = send(&home, SHELL, items, &|args| self.run(args));
            let _ = std::fs::remove_dir_all(home);
            result
        }

        fn subcommands(&self) -> Vec<String> {
            self.calls.borrow().iter().map(|c| c[0].clone()).collect()
        }
    }

    #[test]
    fn a_batch_without_a_text_then_key_boundary_is_one_status_read_and_one_send() {
        let pane = format!("{SHELL}:0.0");
        for items in [
            vec![Item::Key(Key::Enter), text("ls")],
            vec![text("ls"), text("-la")],
            vec![Item::Key(Key::Up), Item::Key(Key::Enter)],
        ] {
            let tmux = Recorder::new("0|0\n");
            tmux.send(&items).unwrap();
            assert_eq!(tmux.subcommands(), ["display-message", "send-keys"]);
            assert_eq!(tmux.calls.borrow()[1], tmux_invocation(&pane, &items));
            // No pause anywhere: the whole batch is two quick tmux calls.
            assert!(tmux.gaps().iter().all(|gap| *gap < KEY_AFTER_TEXT_PAUSE));
        }
    }

    #[test]
    fn a_key_after_text_is_sent_on_its_own_after_the_pause() {
        let pane = format!("{SHELL}:0.0");
        let tmux = Recorder::new("0|0\n");
        let items = [text("ls;"), Item::Key(Key::Enter)];
        tmux.send(&items).unwrap();
        assert_eq!(
            tmux.subcommands(),
            ["display-message", "send-keys", "send-keys"]
        );
        assert_eq!(
            tmux.calls.borrow()[1..],
            [
                strings(&["send-keys", "-t", &pane, "-l", "--", "ls\\;"]),
                strings(&["send-keys", "-t", &pane, "Enter"]),
            ]
        );
        let gaps = tmux.gaps();
        assert!(gaps[0] < KEY_AFTER_TEXT_PAUSE, "no pause before the text");
        assert!(gaps[1] >= KEY_AFTER_TEXT_PAUSE, "{:?}", gaps[1]);
        // A failure in the second part is a plain, uncertain error.
        let mut tmux = Recorder::new("0|0\n");
        tmux.fail = Some("send-keys");
        let error = tmux.send(&items).unwrap_err();
        assert_eq!(error, "send-keys failed");
        assert_eq!(tmux.subcommands(), ["display-message", "send-keys"]);
    }

    #[test]
    fn the_plan_splits_only_where_a_key_directly_follows_text() {
        let up = || Item::Key(Key::Up);
        let enter = || Item::Key(Key::Enter);
        let tab = || Item::Key(Key::Tab);
        let plan = |items: &[Item]| tmux_plan("P", items);
        let one = |items: &[Item]| vec![tmux_invocation("P", items)];
        // Text then Enter: two invocations.
        assert_eq!(
            plan(&[text("ls"), enter()]),
            [
                strings(&["send-keys", "-t", "P", "-l", "--", "ls"]),
                strings(&["send-keys", "-t", "P", "Enter"]),
            ]
        );
        // A lone key, consecutive texts and consecutive keys stay whole.
        for items in [
            vec![enter()],
            vec![text("a")],
            vec![text("a"), text("b")],
            vec![up(), enter(), tab()],
            vec![enter(), text("a")],
            vec![up(), enter(), text("a"), text("b")],
        ] {
            assert_eq!(plan(&items), one(&items), "{items:?}");
        }
        assert_eq!(
            plan(&[text("a"), text("b"), enter()]),
            [
                tmux_invocation("P", &[text("a"), text("b")]),
                tmux_invocation("P", &[enter()]),
            ]
        );
        // [text, Enter, text, Tab]: a pause before each key that follows text,
        // none between the Enter and the text after it.
        assert_eq!(
            plan(&[text("a"), enter(), text("b"), tab()]),
            [
                tmux_invocation("P", &[text("a")]),
                tmux_invocation("P", &[enter(), text("b")]),
                tmux_invocation("P", &[tab()]),
            ]
        );
        assert_eq!(
            plan(&[up(), text("a"), enter(), up(), tab(), text("b")]),
            [
                tmux_invocation("P", &[up(), text("a")]),
                tmux_invocation("P", &[enter(), up(), tab(), text("b")]),
            ]
        );
        assert!(plan(&[]).is_empty());
    }

    #[test]
    fn the_plan_keeps_every_item_in_order_and_the_worst_case_is_quick_enough() {
        // Whatever the split, the invocations together hold the same commands
        // in the same order as the single invocation would.
        let items = vec![
            text("a;"),
            Item::Key(Key::Enter),
            Item::Key(Key::Up),
            text("b"),
            text("c\\"),
            Item::Key(Key::Tab),
            text(";"),
            Item::Key(Key::Ctrl(b'c')),
        ];
        let joined = tmux_plan("P", &items).join(&";".to_owned());
        let whole = tmux_invocation("P", &items);
        // Adjacent key runs are merged in the whole invocation but split at a
        // pause, so compare the tmux key and text arguments, not the commands.
        let words = |args: &[String]| -> Vec<String> {
            args.iter()
                .filter(|a| !matches!(a.as_str(), ";" | "send-keys" | "-t" | "P" | "-l" | "--"))
                .cloned()
                .collect()
        };
        assert_eq!(words(&joined), words(&whole));
        // 64 items that alternate text and keys are 32 pauses: well inside the
        // connector's 15 s limit for one CLI call.
        let worst: Vec<Item> = (0..MAX_ITEMS)
            .map(|index| {
                if index % 2 == 0 {
                    text("a")
                } else {
                    Item::Key(Key::Enter)
                }
            })
            .collect();
        let pauses = tmux_plan("P", &worst).len() - 1;
        assert_eq!(pauses, MAX_ITEMS / 2);
        assert!(KEY_AFTER_TEXT_PAUSE * pauses as u32 <= Duration::from_secs(6));
    }

    #[test]
    fn copy_mode_is_cancelled_before_any_key_and_input_off_sends_nothing() {
        let tmux = Recorder::new("1|0\n");
        tmux.send(&[Item::Key(Key::Enter)]).unwrap();
        assert_eq!(
            tmux.subcommands(),
            ["display-message", "send-keys", "send-keys"]
        );
        let calls = tmux.calls.borrow();
        assert_eq!(
            calls[1],
            strings(&["send-keys", "-t", &format!("{SHELL}:0.0"), "-X", "cancel"])
        );
        drop(calls);

        for status in ["0|1\n", "1|1\n"] {
            let tmux = Recorder::new(status);
            let error = tmux.send(&[Item::Key(Key::Enter)]).unwrap_err();
            assert!(error.starts_with(INPUT_UNAVAILABLE), "{error}");
            assert_eq!(tmux.subcommands(), ["display-message"]);
        }
    }

    #[test]
    fn failures_before_the_send_say_so_and_a_failed_send_does_not() {
        let mut tmux = Recorder::new("0|0\n");
        tmux.fail = Some("display-message");
        let error = tmux.send(&[Item::Key(Key::Enter)]).unwrap_err();
        assert!(error.starts_with(NOT_SENT), "{error}");

        let mut tmux = Recorder::new("0|0\n");
        tmux.fail = Some("send-keys");
        let error = tmux.send(&[Item::Key(Key::Enter)]).unwrap_err();
        assert_eq!(error, "send-keys failed");

        // A cancel that fails while the pane is still in a mode sends nothing;
        // one that fails because the mode just ended carries on.
        let mut tmux = Recorder::new("1|0\n");
        tmux.fail_cancel = true;
        let error = tmux.send(&[Item::Key(Key::Enter)]).unwrap_err();
        assert!(error.starts_with(NOT_SENT), "{error}");
        assert!(error.contains("copy mode"), "{error}");
        assert_eq!(tmux.subcommands().last().unwrap(), "display-message");
        let mut tmux = Recorder::new("1|0\n");
        tmux.statuses.push("0\n");
        tmux.fail_cancel = true;
        tmux.send(&[Item::Key(Key::Enter)]).unwrap();
        assert_eq!(tmux.subcommands().last().unwrap(), "send-keys");

        for status in ["", "garbage", "2|0", "0|", "|0", "0|0|0"] {
            let tmux = Recorder::new(status);
            let error = tmux.send(&[Item::Key(Key::Enter)]).unwrap_err();
            assert!(error.starts_with(NOT_SENT), "{status}: {error}");
            assert_eq!(tmux.subcommands(), ["display-message"]);
        }

        let tmux = Recorder::new("0|0\n");
        let error = tmux.send(&[]).unwrap_err();
        assert!(error.starts_with(INVALID_REQUEST), "{error}");
        assert!(tmux.calls.borrow().is_empty());
    }
}
