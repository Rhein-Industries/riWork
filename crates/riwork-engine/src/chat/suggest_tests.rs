use super::*;
use serde_json::Value;
use std::fs;

/// The rules the phone's tests check too (`ios/Tests/ComposerSuggestionsTests.swift`).
const SHARED_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../ios/Tests/Fixtures/composer-suggestions.json"
);

fn fixture() -> Value {
    serde_json::from_str(&fs::read_to_string(SHARED_FIXTURE).unwrap()).unwrap()
}

/// `text` without the caret mark, and the byte offset where it was.
fn caret(text: &str, mark: &str) -> (String, usize) {
    let at = text.find(mark).expect("every vector marks its caret");
    (text.replacen(mark, "", 1), at)
}

fn entry(value: &Value) -> FileEntry {
    serde_json::from_value(value.clone()).unwrap()
}

fn strings(value: &Value) -> Vec<String> {
    serde_json::from_value(value.clone()).unwrap()
}

fn temp() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("riwork-suggest-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn paths(listing: &Listing) -> Vec<&str> {
    listing.entries.iter().map(|e| e.path.as_str()).collect()
}

// ---- The shared rules ------------------------------------------------------------------------

#[test]
fn the_trigger_rule_matches_the_shared_vectors() {
    let fixture = fixture();
    let mark = fixture["caret"].as_str().unwrap();
    for case in fixture["triggers"].as_array().unwrap() {
        let (text, at) = caret(case["text"].as_str().unwrap(), mark);
        let got = trigger(&text, at);
        let expect = &case["expect"];
        match (got, expect) {
            (None, Value::Null) => {}
            (Some(got), Value::Object(expect)) => {
                let sigil: Sigil = serde_json::from_value(expect["sigil"].clone()).unwrap();
                assert_eq!(got.sigil, sigil, "{text:?}");
                assert_eq!(got.query, expect["query"].as_str().unwrap(), "{text:?}");
                assert_eq!(
                    &text[got.start..got.end],
                    expect["token"].as_str().unwrap(),
                    "{text:?}"
                );
            }
            (got, expect) => panic!("{text:?}: got {got:?}, expected {expect}"),
        }
    }
}

#[test]
fn accepting_matches_the_shared_vectors() {
    let fixture = fixture();
    let mark = fixture["caret"].as_str().unwrap();
    for case in fixture["accept"].as_array().unwrap() {
        let (text, at) = caret(case["text"].as_str().unwrap(), mark);
        let found = trigger(&text, at).expect("an accept vector is in a token");
        let (out, caret_at) = accept(&text, &found, case["insert"].as_str().unwrap());
        let mut shown = out.clone();
        shown.insert_str(caret_at, mark);
        assert_eq!(shown, case["expect"].as_str().unwrap(), "{text:?}");
    }
}

#[test]
fn the_registry_is_what_the_shared_fixture_says() {
    let fixture = fixture();
    for (provider, name) in [
        (Provider::Claude, "claude"),
        (Provider::Codex, "codex"),
        (Provider::Muse, "muse"),
    ] {
        assert_eq!(
            serde_json::to_value(commands(provider)).unwrap(),
            fixture["commands"][name],
            "{name}'s commands: change the fixture with the registry, and the phone's tests see it"
        );
    }
}

#[test]
fn commands_rank_as_the_shared_vectors_say() {
    let fixture = fixture();
    let table = &fixture["command_ranking"];
    let commands: Vec<Command> = strings(&table["commands"])
        .into_iter()
        .map(|name| Command {
            name,
            summary: String::new(),
            argument: None,
            sends: String::new(),
            handler: Handler::Provider,
        })
        .collect();
    for case in table["cases"].as_array().unwrap() {
        let query = case["query"].as_str().unwrap();
        let got: Vec<_> = rank_commands(query, &commands)
            .into_iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(got, strings(&case["expect"]), "query {query:?}");
    }
}

#[test]
fn files_rank_as_the_shared_vectors_say() {
    let fixture = fixture();
    let table = &fixture["file_ranking"];
    let entries: Vec<FileEntry> = serde_json::from_value(table["entries"].clone()).unwrap();
    for case in table["cases"].as_array().unwrap() {
        let query = case["query"].as_str().unwrap();
        let limit = case["limit"].as_u64().unwrap() as usize;
        let got: Vec<_> = rank_files(query, &entries, limit)
            .into_iter()
            .map(|e| e.path.clone())
            .collect();
        assert_eq!(got, strings(&case["expect"]), "query {query:?}");
    }
}

#[test]
fn insertions_kinds_and_sizes_match_the_shared_vectors() {
    let fixture = fixture();
    for case in fixture["insertions"].as_array().unwrap() {
        assert_eq!(
            entry(&case["entry"]).insertion(),
            case["insert"].as_str().unwrap()
        );
    }
    for case in fixture["kinds"].as_array().unwrap() {
        assert_eq!(
            file_preview(&entry(&case["entry"]), None).kind,
            case["kind"].as_str().unwrap()
        );
    }
    for case in fixture["sizes"].as_array().unwrap() {
        assert_eq!(
            size_label(case["bytes"].as_u64().unwrap()),
            case["label"].as_str().unwrap()
        );
    }
}

#[test]
fn the_phone_reply_reads_and_writes_as_the_shared_fixture() {
    let fixture = fixture();
    let reply: Reply = serde_json::from_value(fixture["reply"].clone()).unwrap();
    assert_eq!(serde_json::to_value(&reply).unwrap(), fixture["reply"]);
    assert_eq!(reply.commands, commands(reply.provider));
    assert_eq!(reply.file_bound, FILE_BOUND);
}

#[test]
fn a_reply_lists_the_chat_folder_and_the_providers_commands() {
    let root = temp();
    write(&root, "a.txt", "hello");
    let made = reply("chat-1", Provider::Codex, &root);
    assert_eq!(made.chat_id, "chat-1");
    assert_eq!(made.root, root.display().to_string());
    assert!(
        made.commands.is_empty(),
        "Codex reads a typed /compact as words"
    );
    assert_eq!(paths(&made.files), ["a.txt"]);
    assert_eq!(made.files.entries[0].bytes, Some(5));
    let _ = fs::remove_dir_all(root);
}

// ---- Rules that are not in the fixture -----------------------------------------------------

#[test]
fn the_largest_size_has_a_label() {
    assert_eq!(size_label(u64::MAX), "17592186044416.0 MB");
}

#[test]
fn a_caret_off_a_character_boundary_or_past_the_end_opens_nothing() {
    assert_eq!(trigger("@é", 2), None);
    assert_eq!(trigger("@a", 3), None);
    assert!(trigger("@é", 3).is_some());
}

#[test]
fn every_registered_command_is_one_lowercase_word_offered_somewhere() {
    let mut seen = std::collections::HashSet::new();
    for command in COMMANDS {
        assert!(
            seen.insert(command.name),
            "{} is registered twice",
            command.name
        );
        assert!(
            !command.providers.is_empty(),
            "{} reaches no chat",
            command.name
        );
        assert!(
            !command.name.is_empty()
                && command
                    .name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{:?} is not a command name the trigger can complete",
            command.name
        );
        assert!(!command.summary.is_empty() && !command.sends.is_empty());
    }
}

#[test]
fn only_claude_chats_offer_compact() {
    assert_eq!(
        commands(Provider::Claude)
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["compact"]
    );
    assert!(commands(Provider::Codex).is_empty());
    assert!(commands(Provider::Muse).is_empty());
}

#[test]
fn a_command_preview_says_what_it_is_and_what_sending_does() {
    let compact = &commands(Provider::Claude)[0];
    let preview = command_preview(compact, Provider::Claude);
    assert_eq!(preview.title, "/compact");
    assert_eq!(preview.kind, "Claude command");
    assert_eq!(
        preview.body,
        [
            "Summarize the conversation so far to free up context",
            "Takes no argument"
        ]
    );
    assert_eq!(preview.inserts, "/compact");
    assert!(
        preview
            .effect
            .starts_with("Sending: Goes to Claude as typed.")
    );
    let riwork = Command {
        name: "memo".into(),
        summary: "s".into(),
        argument: Some("TEXT".into()),
        sends: "x".into(),
        handler: Handler::Riwork,
    };
    let preview = command_preview(&riwork, Provider::Codex);
    assert_eq!(preview.kind, "RiWork command");
    assert_eq!(preview.body[1], "Takes TEXT");
}

#[test]
fn a_file_preview_shows_its_first_lines_and_says_nothing_is_attached() {
    let file = FileEntry {
        path: "src/main.rs".into(),
        dir: false,
        bytes: Some(2048),
    };
    let preview = file_preview(&file, Some(&Peek::Text(vec!["fn main() {".into()])));
    assert_eq!(preview.title, "src/main.rs");
    assert_eq!(preview.kind, "File · 2 KB");
    assert_eq!(preview.body, ["fn main() {"]);
    assert_eq!(preview.inserts, "src/main.rs");
    assert!(preview.effect.contains("nothing is attached or sent"));
    assert_eq!(
        file_preview(&file, Some(&Peek::Text(Vec::new()))).body,
        ["(empty)"]
    );
    assert_eq!(file_preview(&file, Some(&Peek::Binary)).body, ["Not text"]);
    assert!(file_preview(&file, None).body.is_empty());
}

// ---- The listing ---------------------------------------------------------------------------

#[test]
fn a_listing_is_breadth_first_and_leaves_out_what_git_ignores() {
    let root = temp();
    write(&root, ".gitignore", "target/\n*.log\n!keep.log\n");
    write(&root, "README.md", "readme");
    write(&root, "build.log", "x");
    write(&root, "keep.log", "x");
    write(&root, "target/debug/app", "binary");
    write(&root, "src/main.rs", "fn main() {}");
    write(&root, "src/deep/inner/file.rs", "");
    write(&root, "src/.gitignore", "generated.rs\n");
    write(&root, "src/generated.rs", "");
    write(&root, ".git/HEAD", "ref");
    write(&root, ".github/workflows/ci.yml", "on: push");
    write(&root, ".DS_Store", "");
    std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
    std::os::unix::fs::symlink(root.join("README.md"), root.join("readme-link")).unwrap();
    let listing = list_files(&root);
    assert!(!listing.truncated);
    assert_eq!(
        paths(&listing),
        [
            ".github",
            ".gitignore",
            "README.md",
            "keep.log",
            "src",
            ".github/workflows",
            "src/.gitignore",
            "src/deep",
            "src/main.rs",
            ".github/workflows/ci.yml",
            "src/deep/inner",
            "src/deep/inner/file.rs",
        ]
    );
    let main = listing
        .entries
        .iter()
        .find(|e| e.path == "src/main.rs")
        .unwrap();
    assert_eq!((main.dir, main.bytes), (false, Some(12)));
    let src = listing.entries.iter().find(|e| e.path == "src").unwrap();
    assert_eq!((src.dir, src.bytes), (true, None));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_listing_stops_at_its_bounds_and_says_so() {
    let root = temp();
    for n in 0..10 {
        write(&root, &format!("f{n}.txt"), "");
    }
    write(&root, "d1/d2/d3/deep.txt", "");
    let bounded = |files, scanned, depth| {
        list_files_within(
            &root,
            Bounds {
                files,
                scanned,
                budget: Duration::from_secs(5),
                depth,
            },
        )
    };
    let few = bounded(4, 1000, 24);
    assert!(few.truncated);
    assert_eq!(few.entries.len(), 4);
    let scanned = bounded(1000, 3, 24);
    assert!(scanned.truncated);
    assert_eq!(scanned.entries.len(), 3);
    let shallow = bounded(1000, 1000, 2);
    assert!(shallow.truncated, "d1/d2 is listed but not read");
    assert!(paths(&shallow).contains(&"d1/d2"));
    assert!(!paths(&shallow).contains(&"d1/d2/d3"));
    let all = bounded(1000, 1000, 24);
    assert!(!all.truncated);
    assert_eq!(all.entries.len(), 14);
    let none = list_files_within(
        &root,
        Bounds {
            budget: Duration::ZERO,
            ..Bounds::DEFAULT
        },
    );
    assert!(none.truncated, "a spent budget stops the walk");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_folder_that_cannot_be_read_lists_nothing() {
    let listing = list_files(Path::new("/nonexistent/riwork/suggest"));
    assert_eq!(listing, Listing::default());
}

#[test]
fn a_cached_listing_is_fresh_for_its_time_to_live() {
    let mut cache = FileCache::default();
    let root = PathBuf::from("/work");
    let read = Instant::now();
    assert!(cache.fresh(&root, read).is_none());
    cache.put(root.clone(), read, Arc::new(Listing::default()));
    assert!(cache.fresh(&root, read + FILE_TTL / 2).is_some());
    assert!(cache.fresh(&root, read + FILE_TTL).is_none());
    assert!(cache.any(&root).is_some(), "a stale listing is still shown");
    assert!(cache.fresh(Path::new("/other"), read).is_none());
}

// ---- Peeking -------------------------------------------------------------------------------

#[test]
fn a_peek_shows_the_first_lines_of_a_text_file() {
    let root = temp();
    let long = "x".repeat(400);
    write(
        &root,
        "a.rs",
        &format!("one\n\ttwo\n{long}\n4\n5\n6\n7\n8\n"),
    );
    let Peek::Text(lines) = peek(&root, "a.rs") else {
        panic!("text");
    };
    assert_eq!(lines.len(), PEEK_LINES);
    assert_eq!(lines[0], "one");
    assert_eq!(lines[1], "    two");
    assert_eq!(lines[2].chars().count(), PEEK_LINE_CHARS);
    assert!(lines[2].ends_with('…'));
    write(&root, "empty", "");
    assert_eq!(peek(&root, "empty"), Peek::Text(Vec::new()));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_peek_reads_no_further_than_its_bound() {
    let root = temp();
    let line = "y".repeat(100);
    write(&root, "big.txt", &format!("{line}\n").repeat(1000));
    let Peek::Text(lines) = peek(&root, "big.txt") else {
        panic!("text");
    };
    assert_eq!(lines.len(), PEEK_LINES);
    // A multi-byte character cut by the read's end is not "binary".
    let mut bytes = "é".repeat(PEEK_BYTES / 2).into_bytes();
    bytes.truncate(PEEK_BYTES - 1);
    assert!(matches!(Peek::from_bytes(&bytes), Peek::Text(_)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn binary_files_and_paths_outside_the_folder_are_not_shown() {
    let root = temp();
    fs::write(root.join("bin"), [0x89, b'P', b'N', b'G', 0, 1, 2]).unwrap();
    assert_eq!(peek(&root, "bin"), Peek::Binary);
    fs::write(root.join("latin1"), [b'a', 0xE9, b'b']).unwrap();
    assert_eq!(peek(&root, "latin1"), Peek::Binary);
    let outside = temp();
    write(&outside, "secret.txt", "secret");
    std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link")).unwrap();
    assert_eq!(peek(&root, "link"), Peek::Unreadable);
    assert_eq!(peek(&root, "../secret.txt"), Peek::Unreadable);
    assert_eq!(
        peek(&root, &outside.join("secret.txt").display().to_string()),
        Peek::Unreadable
    );
    fs::create_dir_all(root.join("dir")).unwrap();
    assert_eq!(peek(&root, "dir"), Peek::Unreadable);
    assert_eq!(peek(&root, "missing"), Peek::Unreadable);
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(outside);
}
