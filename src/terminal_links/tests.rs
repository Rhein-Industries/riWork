use super::*;
use crate::theme::{GhosttyPadding, PaddingBalance};
use uuid::Uuid;

fn chars(text: &str) -> Vec<char> {
    text.chars().collect()
}

/// What is under the middle of the first `needle` in `line`.
fn under(line: &str, needle: &str) -> Vec<Candidate> {
    let chars = chars(line);
    let start = line.find(needle).expect("needle in line");
    let start = line[..start].chars().count();
    candidates_at(&chars, start + needle.chars().count() / 2)
}

fn path(text: &str, line: Option<u32>, col: Option<u32>) -> (String, Option<u32>, Option<u32>) {
    (text.to_owned(), line, col)
}

/// The paths among the candidates, as (text, line, col).
fn paths(candidates: &[Candidate]) -> Vec<(String, Option<u32>, Option<u32>)> {
    candidates
        .iter()
        .filter_map(|candidate| match candidate {
            Candidate::Path {
                text, line, col, ..
            } => Some((text.clone(), *line, *col)),
            Candidate::Url { .. } => None,
        })
        .collect()
}

fn url(candidates: &[Candidate]) -> Option<&str> {
    match candidates {
        [Candidate::Url { url, .. }] => Some(url),
        _ => None,
    }
}

// ---- the modifiers ----------------------------------------------------------------------

#[test]
fn only_command_and_command_shift_make_a_link_gesture() {
    let command = gpui::Modifiers::command();
    assert_eq!(open_mode(command), Some(OpenMode::Open));
    assert_eq!(
        open_mode(gpui::Modifiers {
            shift: true,
            ..command
        }),
        Some(OpenMode::Edit)
    );
    assert_eq!(open_mode(gpui::Modifiers::default()), None);
    assert_eq!(open_mode(gpui::Modifiers::shift()), None);
    // Chords with Control or Option belong to the terminal.
    for chord in [
        gpui::Modifiers {
            control: true,
            ..command
        },
        gpui::Modifiers {
            alt: true,
            ..command
        },
        gpui::Modifiers {
            function: true,
            ..command
        },
    ] {
        assert_eq!(open_mode(chord), None);
    }
}

// ---- pointer to cell --------------------------------------------------------------------

const DEFAULT_PADDING: GhosttyPadding = GhosttyPadding::DEFAULT;

fn size(available: u32, cells: u32) -> f64 {
    cell_size(SizeRange::of(available, cells), available, cells)
}

#[test]
fn a_cell_is_the_whole_pixel_size_that_fits_the_grid() {
    // 1000 pt wide at 2x with 2 pt of padding each side leaves 1992 px, in which 124 cells of 16
    // px fit (and the 8 px left over are not a cell). Dividing by the count would say 16.06 and
    // put the last column 7 px off.
    assert_eq!(size(1992, 124), 16.0);
    // 1192 px of rows of 34: 35 fit.
    assert_eq!(size(1192, 35), 34.0);
    // A short axis allows two sizes (34 and 35 both give 17 rows in 600 px), so the middle is
    // taken to keep the far end within half a pixel per cell.
    assert_eq!(size(600, 17), 34.5);
    // A count that no size gives falls back to division.
    assert_eq!(size(10, 20), 0.5);
}

#[test]
fn a_taller_terminal_settles_the_cell_size_of_a_short_one() {
    let explicit = explicit_padding(&DEFAULT_PADDING, 2.0);
    let rows = |hint: &mut CellHint, height: u32, rows: u32| {
        grid_geometry(
            (2000, height),
            124,
            rows,
            explicit,
            PaddingBalance::False,
            hint,
        )
        .unwrap()
        .cell_height
    };
    // On its own, 21 rows in 747 px of space could be rows of 34 or of 35.
    let mut alone = CellHint::default();
    assert_eq!(rows(&mut alone, 755, 21), 34.5);
    // After a terminal with room for 35 rows of 34 px, it is known.
    let mut hint = CellHint::default();
    hint.for_scale(2.0);
    assert_eq!(rows(&mut hint, 1200, 35), 34.0);
    assert_eq!(rows(&mut hint, 755, 21), 34.0);
    // A terminal that contradicts them (a zoomed font) starts the record again, and the next
    // one is compared with that.
    assert_eq!(rows(&mut hint, 1000, 20), 48.5);
    assert_eq!(rows(&mut hint, 755, 15), 48.5);
    // Another display scale knows nothing of these sizes.
    hint.for_scale(1.0);
    assert_eq!(
        hint,
        CellHint {
            scale: 1000,
            ..CellHint::default()
        }
    );
}

#[test]
fn grid_and_pointer_follow_ghosttys_padding() {
    // 1000 x 600 pt at 2x: 2000 x 1200 px, 4 px padding, cells of 16 x 34.
    let size = (1000.0, 600.0);
    let cells = (124, 35);
    let cell = |x_px: f64, y_px: f64| {
        cell_under_pointer(
            size,
            (x_px / 2.0, y_px / 2.0),
            2.0,
            cells,
            &DEFAULT_PADDING,
            &mut CellHint::default(),
        )
    };
    // The first cell begins after the padding, not at the edge.
    assert_eq!(cell(4.0, 4.0), Some((0, 0)));
    assert_eq!(cell(3.9, 10.0), None);
    assert_eq!(cell(10.0, 3.9), None);
    assert_eq!(cell(19.9, 37.9), Some((0, 0)));
    assert_eq!(cell(20.0, 38.0), Some((1, 1)));
    // Cell 10, row 5: x = 4 + 16 * 10 + 8, y = 4 + 34 * 5 + 17.
    assert_eq!(cell(172.0, 191.0), Some((10, 5)));
    // The last column, whose position division by the count would get wrong.
    assert_eq!(cell(4.0 + 16.0 * 123.0 + 15.0, 100.0), Some((123, 2)));
    // Past the last cell: the 8 px left over and the padding.
    assert_eq!(cell(4.0 + 16.0 * 124.0, 100.0), None);
    assert_eq!(cell(100.0, 4.0 + 34.0 * 35.0), None);
}

#[test]
fn balanced_padding_centres_the_grid() {
    let screen = (2000, 1200);
    let explicit = explicit_padding(&DEFAULT_PADDING, 2.0);
    let plain = grid_geometry(
        screen,
        124,
        35,
        explicit,
        PaddingBalance::False,
        &mut CellHint::default(),
    )
    .unwrap();
    assert_eq!((plain.origin_x, plain.origin_y), (4.0, 4.0));
    // 2000 - 124 * 16 = 16 left over: 8 each side. 1200 - 35 * 34 = 10: 5 each side.
    let equal = grid_geometry(
        screen,
        124,
        35,
        explicit,
        PaddingBalance::Equal,
        &mut CellHint::default(),
    )
    .unwrap();
    assert_eq!((equal.origin_x, equal.origin_y), (8.0, 5.0));
    // `true` caps the top: half a cell plus the horizontal padding is 12 px, so 5 stands.
    let capped = grid_geometry(
        screen,
        124,
        35,
        explicit,
        PaddingBalance::True,
        &mut CellHint::default(),
    )
    .unwrap();
    assert_eq!((capped.origin_x, capped.origin_y), (8.0, 5.0));
    // With nearly a whole row left over (1223 px of rows of 34 hold 35 and 33 more), `equal`
    // gives the top 20 px and `true` stops at half a cell plus the side padding, 12.
    let tall = grid_geometry(
        (2000, 1231),
        124,
        35,
        explicit,
        PaddingBalance::Equal,
        &mut CellHint::default(),
    )
    .unwrap();
    assert_eq!(tall.origin_y, 20.0);
    let tall = grid_geometry(
        (2000, 1231),
        124,
        35,
        explicit,
        PaddingBalance::True,
        &mut CellHint::default(),
    )
    .unwrap();
    assert_eq!(tall.origin_y, 12.0);
}

#[test]
fn a_surface_too_small_for_its_grid_has_no_cells() {
    let explicit = explicit_padding(&DEFAULT_PADDING, 2.0);
    assert!(
        grid_geometry(
            (10, 10),
            80,
            24,
            explicit,
            PaddingBalance::False,
            &mut CellHint::default()
        )
        .is_none()
    );
    assert!(
        grid_geometry(
            (2000, 1200),
            0,
            24,
            explicit,
            PaddingBalance::False,
            &mut CellHint::default()
        )
        .is_none()
    );
    assert!(
        grid_geometry(
            (4, 4),
            1,
            1,
            explicit,
            PaddingBalance::False,
            &mut CellHint::default()
        )
        .is_none()
    );
}

// ---- tokens -----------------------------------------------------------------------------

#[test]
fn file_positions_in_every_form_agents_print_them() {
    let line = "  Updated src/main.rs:120:5 to read the file";
    // The position is read off first; the name whole is the fallback for a file that is called
    // `main.rs:120:5`.
    assert_eq!(
        paths(&under(line, "src/main.rs")),
        [
            path("src/main.rs", Some(120), Some(5)),
            path("src/main.rs:120:5", None, None)
        ]
    );
    for (text, expected) in [
        ("src/main.rs:120", path("src/main.rs", Some(120), None)),
        ("src/main.rs:120-135", path("src/main.rs", Some(120), None)),
        ("src/main.rs#L120", path("src/main.rs", Some(120), None)),
        (
            "src/main.rs#L120C7",
            path("src/main.rs", Some(120), Some(7)),
        ),
        (
            "src/main.rs#L120-L135",
            path("src/main.rs", Some(120), None),
        ),
        ("src/main.rs#install", path("src/main.rs", None, None)),
        ("src/main.rs", path("src/main.rs", None, None)),
    ] {
        let line = format!("see {text} now");
        assert_eq!(paths(&under(&line, text))[0], expected, "{text}");
    }
    // The colon that ends a sentence is not part of the path or the position.
    assert_eq!(
        paths(&under("error in src/a.rs:9:", "src/a.rs"))[0],
        path("src/a.rs", Some(9), None)
    );
    assert_eq!(
        paths(&under("error in src/a.rs: oops", "src/a.rs"))[0],
        path("src/a.rs", None, None)
    );
}

#[test]
fn test_ids_and_tracebacks_name_the_file_and_what_else() {
    // pytest: the file, then what to run in it.
    assert_eq!(
        paths(&under(
            "FAILED tests/test_a.py::TestA::test_b - boom",
            "test_a"
        ))[0],
        path("tests/test_a.py", None, None)
    );
    // Python: the line follows the quoted name.
    assert_eq!(
        paths(&under("  File \"/app/main.py\", line 12, in run", "main"))[0],
        path("/app/main.py", Some(12), None)
    );
    assert_eq!(
        paths(&under("File \"/my app/main.py\", line 7", "app"))[0],
        path("/my app/main.py", Some(7), None)
    );
    // Without the quote on both sides, `, line` means nothing.
    assert_eq!(
        paths(&under("see /app/main.py, line 12", "main"))[0],
        path("/app/main.py", None, None)
    );
}

#[test]
fn paths_in_backticks_quotes_brackets_and_box_borders() {
    assert_eq!(
        paths(&under("Modified `ios/Core/X.swift` and more", "Core"))[0],
        path("ios/Core/X.swift", None, None)
    );
    assert_eq!(
        paths(&under("(see docs/a.md)", "docs/a.md"))[0],
        path("docs/a.md", None, None)
    );
    assert_eq!(
        paths(&under("opened 'a/b.txt'.", "a/b.txt"))[0],
        path("a/b.txt", None, None)
    );
    assert_eq!(
        paths(&under("│ src/lib.rs │", "src/lib.rs"))[0],
        path("src/lib.rs", None, None)
    );
    assert_eq!(
        paths(&under("● Update(src/lib.rs)", "src/lib.rs")),
        [
            path("Update(src/lib.rs)", None, None),
            path("src/lib.rs", None, None)
        ]
    );
    // A Markdown link: the whole thing first, then the path in the parentheses.
    assert_eq!(
        paths(&under("[docs](docs/a.md)", "docs/a.md")),
        [
            path("[docs](docs/a.md)", None, None),
            path("docs/a.md", None, None)
        ]
    );
    // Brackets that belong to the path.
    assert_eq!(
        paths(&under("app/(auth)/[id]/page.tsx", "page"))[0],
        path("app/(auth)/[id]/page.tsx", None, None)
    );
    assert_eq!(
        paths(&under("--config=conf/app.toml", "app"))[0],
        path("conf/app.toml", None, None)
    );
}

#[test]
fn a_path_with_spaces_is_found_inside_its_quotes() {
    let line = "open \"/Users/me/My Docs/notes v2.md\" now";
    let found = paths(&under(line, "Docs"));
    // The quoted run comes first; the word under the pointer cannot be a path on its own.
    assert_eq!(found[0], path("/Users/me/My Docs/notes v2.md", None, None));
    let line = "wrote `docs/Design Notes.md:14` today";
    assert_eq!(
        paths(&under(line, "Notes"))[0],
        path("docs/Design Notes.md", Some(14), None)
    );
    // Pointing at a space inside the quotes still finds it.
    let space = line.find("Design Notes").unwrap() + "Design".len();
    assert_eq!(
        paths(&candidates_at(&chars(line), space))[0],
        path("docs/Design Notes.md", Some(14), None)
    );
}

#[test]
fn starting_points_of_a_path() {
    for (line, needle, expected) in [
        ("cd ~/work/app now", "work", "~/work/app"),
        ("cd ./build/out now", "build", "./build/out"),
        ("cat ../shared/a.txt now", "shared", "../shared/a.txt"),
        ("cat /etc/hosts now", "hosts", "/etc/hosts"),
        ("edit .gitignore now", "gitignore", ".gitignore"),
        ("edit README.md. Then", "README", "README.md"),
        ("ls docs/ now", "docs", "docs/"),
    ] {
        assert_eq!(
            paths(&under(line, needle))[0],
            path(expected, None, None),
            "{line}"
        );
    }
}

#[test]
fn words_and_numbers_are_not_paths() {
    for (line, needle) in [
        ("version v1.2.3 is out", "1.2"),
        ("pi is 3.14 or so", "3.14"),
        ("the quick brown fox", "quick"),
        ("at 12:30 today", "12:30"),
        ("on localhost:3000 now", "localhost"),
        ("a - b", "-"),
    ] {
        assert!(paths(&under(line, needle)).is_empty(), "{line}");
    }
    // Nothing under a space or past the end of the line.
    assert!(candidates_at(&chars("a b"), 1).is_empty());
    assert!(candidates_at(&chars("a b"), 9).is_empty());
    assert!(candidates_at(&[], 0).is_empty());
}

#[test]
fn urls_end_where_sentences_and_brackets_do() {
    assert_eq!(
        url(&under(
            "merged https://github.com/x/y/pull/1. Next",
            "github"
        )),
        Some("https://github.com/x/y/pull/1")
    );
    assert_eq!(
        url(&under("(https://example.com)", "example")),
        Some("https://example.com")
    );
    assert_eq!(
        url(&under("[a](https://example.com/a?b=1)", "example")),
        Some("https://example.com/a?b=1")
    );
    assert_eq!(
        url(&under(
            "see https://en.wikipedia.org/wiki/Rust_(video_game).",
            "wiki"
        )),
        Some("https://en.wikipedia.org/wiki/Rust_(video_game)")
    );
    assert_eq!(
        url(&under("go to <https://example.com/x>, or not", "example")),
        Some("https://example.com/x")
    );
    assert_eq!(
        url(&under("'http://localhost:3000/a'", "localhost")),
        Some("http://localhost:3000/a")
    );
    assert_eq!(
        url(&under("write mailto:me@example.com!", "me@")),
        Some("mailto:me@example.com")
    );
    // Either half of a URL inside a URL is the whole URL.
    let nested = "https://a.example/?next=https://b.example/x";
    assert_eq!(url(&under(nested, "a.example")), Some(nested));
    assert_eq!(url(&under(nested, "b.example")), Some(nested));
    // Pointing before the URL, or at the bracket that ends it, finds nothing.
    assert!(url(&candidates_at(&chars("(https://example.com)"), 0)).is_none());
    assert!(url(&candidates_at(&chars("(https://example.com)"), 20)).is_none());
    assert!(url(&candidates_at(&chars("(https://example.com)"), 19)).is_some());
}

#[test]
fn a_url_cut_short_on_screen_is_not_opened() {
    assert!(under("see https://example.com/very/lo… more", "example").is_empty());
    // A scheme with nothing after it, and words that merely end in one.
    assert!(under("use https:// here", "https").is_empty());
    assert!(under("xhttps://example.com", "example").is_empty());
}

#[test]
fn a_file_url_is_a_path() {
    assert_eq!(
        paths(&under("open file:///tmp/a", "tmp")),
        [path("/tmp/a", None, None)]
    );
    assert_eq!(
        paths(&under("open file:///tmp/My%20Docs/a.md#L12.", "tmp")),
        [path("/tmp/My Docs/a.md", Some(12), None)]
    );
    assert_eq!(
        paths(&under("file://localhost/tmp/a", "tmp")),
        [path("/tmp/a", None, None)]
    );
    // Another machine's file is not this one's.
    assert!(under("file://server/share/a", "share").is_empty());
}

// ---- the file system --------------------------------------------------------------------

struct Tree(PathBuf);

impl Tree {
    fn new() -> Self {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("riwork-links-{}", Uuid::new_v4()));
        for dir in ["work/src", "work/docs", "work/My Docs", "home/proj"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            "work/src/main.rs",
            "work/README.md",
            "work/My Docs/notes.md",
            "home/proj/a.rs",
            "elsewhere.txt",
        ] {
            fs::write(root.join(file), "x\n").unwrap();
        }
        Self(root)
    }

    fn join(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn link_at(tree: &Tree, line: &str, needle: &str, cwd: &str) -> Option<Link> {
    let cwd = tree.join(cwd);
    let home = tree.join("home");
    let root = tree.join("work");
    resolve(
        &under(line, needle),
        &Bases {
            cwd: &cwd,
            root: Some(&root),
            home: Some(&home),
        },
    )
}

fn found(path: PathBuf, line: Option<u32>, col: Option<u32>, is_dir: bool) -> Option<Link> {
    Some(Link::Path(ResolvedPath {
        path,
        line,
        col,
        is_dir,
    }))
}

#[test]
fn a_relative_path_starts_in_the_shells_folder_then_the_project_root() {
    let tree = Tree::new();
    // From the project root.
    assert_eq!(
        link_at(&tree, "src/main.rs:120:5", "main", "work"),
        found(tree.join("work/src/main.rs"), Some(120), Some(5), false)
    );
    // From inside a folder of it: the folder's own file first, else the root's.
    assert_eq!(
        link_at(&tree, "main.rs", "main", "work/src"),
        found(tree.join("work/src/main.rs"), None, None, false)
    );
    assert_eq!(
        link_at(&tree, "see src/main.rs", "main", "work/docs"),
        found(tree.join("work/src/main.rs"), None, None, false)
    );
    assert_eq!(
        link_at(&tree, "../README.md", "README", "work/src"),
        found(tree.join("work/README.md"), None, None, false)
    );
    assert_eq!(
        link_at(&tree, "README.md", "README", "work/src"),
        found(tree.join("work/README.md"), None, None, false)
    );
}

#[test]
fn the_a_and_b_folders_of_a_git_diff_are_the_project() {
    let tree = Tree::new();
    for line in [
        "--- a/src/main.rs",
        "+++ b/src/main.rs",
        "diff --git a/src/main.rs b/src/main.rs",
    ] {
        assert_eq!(
            link_at(&tree, line, "main", "work"),
            found(tree.join("work/src/main.rs"), None, None, false),
            "{line}"
        );
    }
    // A folder really called `a` wins over the guess.
    fs::create_dir_all(tree.join("work/a/src")).unwrap();
    fs::write(tree.join("work/a/src/main.rs"), "").unwrap();
    assert_eq!(
        link_at(&tree, "a/src/main.rs", "main", "work"),
        found(tree.join("work/a/src/main.rs"), None, None, false)
    );
}

#[test]
fn home_absolute_and_quoted_paths() {
    let tree = Tree::new();
    assert_eq!(
        link_at(&tree, "~/proj/a.rs:7", "proj", "work"),
        found(tree.join("home/proj/a.rs"), Some(7), None, false)
    );
    let absolute = tree.join("elsewhere.txt");
    let line = format!("open {}", absolute.display());
    assert_eq!(
        link_at(&tree, &line, "elsewhere", "work"),
        found(absolute, None, None, false)
    );
    assert_eq!(
        link_at(&tree, "wrote \"My Docs/notes.md\" ok", "Docs", "work"),
        found(tree.join("work/My Docs/notes.md"), None, None, false)
    );
}

#[test]
fn a_folder_is_a_link_and_a_missing_path_is_not() {
    let tree = Tree::new();
    assert_eq!(
        link_at(&tree, "in docs/ today", "docs", "work"),
        found(tree.join("work/docs"), None, None, true)
    );
    assert_eq!(link_at(&tree, "src/missing.rs:3", "missing", "work"), None);
    assert_eq!(link_at(&tree, "main.rs", "main", "work/docs"), None);
    assert_eq!(link_at(&tree, "~/proj/none.rs", "proj", "work"), None);
    assert_eq!(link_at(&tree, "/definitely/not/here", "here", "work"), None);
    // The plain reading is tried when the bracketed one is nothing.
    assert_eq!(
        link_at(&tree, "[readme](README.md)", "README", "work"),
        found(tree.join("work/README.md"), None, None, false)
    );
}

#[test]
fn paths_are_settled_the_way_the_system_does() {
    let tree = Tree::new();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(tree.join("work/src"), tree.join("work/link")).unwrap();
        assert_eq!(
            link_at(&tree, "link/main.rs", "main", "work"),
            found(tree.join("work/src/main.rs"), None, None, false)
        );
    }
    assert_eq!(
        link_at(&tree, "docs/../src/main.rs", "main", "work"),
        found(tree.join("work/src/main.rs"), None, None, false)
    );
}

#[test]
fn containment_compares_resolved_paths() {
    let tree = Tree::new();
    let root = tree.join("work");
    assert_eq!(
        within(&root, &tree.join("work/src/main.rs")),
        Some(root.join("src/main.rs"))
    );
    assert_eq!(within(&root, &root), Some(root.clone()));
    assert_eq!(within(&root, &tree.join("elsewhere.txt")), None);
    assert_eq!(within(&root, &tree.join("work-not/x")), None);
    // A root spelled through a link still contains what the shell reports resolved.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&root, tree.join("alias")).unwrap();
        let alias = tree.join("alias");
        assert_eq!(
            within(&alias, &tree.join("work/src/main.rs")),
            Some(alias.join("src/main.rs"))
        );
    }
}

#[test]
fn programs_and_installers_are_not_opened() {
    use std::os::unix::fs::PermissionsExt;
    let tree = Tree::new();
    let script = tree.join("run.sh");
    fs::write(&script, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(open_refusal(&script).is_some());
    let app = tree.join("Thing.APP");
    fs::create_dir(&app).unwrap();
    assert!(open_refusal(&app).is_some());
    let command = tree.join("x.command");
    fs::write(&command, "echo\n").unwrap();
    assert!(open_refusal(&command).is_some());
    assert_eq!(open_refusal(&tree.join("elsewhere.txt")), None);
    assert_eq!(open_refusal(&tree.join("work/docs")), None);
}

#[test]
fn a_hash_or_a_colon_that_belongs_to_the_name_is_not_a_position() {
    let tree = Tree::new();
    fs::create_dir_all(tree.join("work/C#Tools")).unwrap();
    fs::write(tree.join("work/C#Tools/readme.md"), "").unwrap();
    fs::write(tree.join("work/note:1.txt"), "").unwrap();
    // The last `#` starts an anchor only when no `/` follows it.
    assert_eq!(
        link_at(&tree, "see C#Tools/readme.md now", "readme", "work"),
        found(tree.join("work/C#Tools/readme.md"), None, None, false)
    );
    assert_eq!(
        link_at(&tree, "see C#Tools/readme.md#L9 now", "readme", "work"),
        found(tree.join("work/C#Tools/readme.md"), Some(9), None, false)
    );
    // A name that ends in what looks like a position is found whole when nothing else is.
    assert_eq!(
        link_at(&tree, "see note:1.txt now", "note", "work"),
        found(tree.join("work/note:1.txt"), None, None, false)
    );
}

#[test]
fn only_documents_pictures_and_source_files_are_opened_from_a_link() {
    let tree = Tree::new();
    let make = |name: &str| {
        let path = tree.join(name);
        fs::write(&path, "x").unwrap();
        path
    };
    for name in ["a.txt", "a.MD", "a.pdf", "a.png", "a.rs", "a.json", "a.mp4"] {
        assert_eq!(open_refusal(&make(name)), None, "{name}");
    }
    // `open` mounts, installs, and (through Python's launcher) runs these.
    for name in [
        "a.iso",
        "a.dmg",
        "a.pkg",
        "a.mobileconfig",
        "a.py",
        "a.sh",
        "a.command",
        "a.sparseimage",
        "noextension",
    ] {
        assert!(open_refusal(&make(name)).is_some(), "{name}");
    }
    assert!(open_refusal(&tree.join("missing.txt")).is_some());
}

#[test]
fn the_target_of_a_hyperlink_is_what_a_click_opens() {
    let name = Some("MacBook-Pro");
    let target = |uri: &str| match hyperlink_candidate(uri, name) {
        Some(Candidate::Url { url, .. }) => Some(Err(url)),
        Some(Candidate::Path {
            text, line, col, ..
        }) => Some(Ok((text, line, col))),
        None => None,
    };
    assert_eq!(
        target("https://example.com/a?b=1"),
        Some(Err("https://example.com/a?b=1".into()))
    );
    assert_eq!(
        target("mailto:me@example.com"),
        Some(Err("mailto:me@example.com".into()))
    );
    // A file on this machine, with or without its name in it, with a position or an escape.
    assert_eq!(
        target("file:///Users/me/a%20b.rs#L12"),
        Some(Ok(("/Users/me/a b.rs".into(), Some(12), None)))
    );
    assert_eq!(
        target("file://MacBook-Pro.local/Users/me/a.rs"),
        Some(Ok(("/Users/me/a.rs".into(), None, None)))
    );
    assert_eq!(
        target("file://localhost/etc/hosts"),
        Some(Ok(("/etc/hosts".into(), None, None)))
    );
    // Another machine's file, and schemes that are not for a click here.
    assert_eq!(target("file://server/share/a.rs"), None);
    for refused in [
        "ssh://host",
        "vscode://file/a.rs",
        "javascript:alert(1)",
        "x-apple.systempreferences:",
        "",
    ] {
        assert_eq!(target(refused), None, "{refused}");
    }
}

#[test]
fn hyperlinks_are_read_off_the_escaped_capture() {
    let esc = "\u{1b}";
    let row = |text: &str| format!("{text}\n");
    let escaped = [
        // Colors come and go; a link covers the characters between its open and its close.
        row(&format!(
            "{esc}[1mgo{esc}[0m {esc}]8;;https://a.example/x{esc}\\the docs{esc}]8;;{esc}\\ ok"
        )),
        // BEL ends an OSC too, parameters before the target are skipped, and a charset switch is
        // not text.
        row(&format!(
            "{esc}(B{esc}]8;id=7;file:///tmp/b.rs\u{7}b.rs{esc}]8;;\u{7}"
        )),
        row("plain"),
    ]
    .concat();
    let (links, targets) = parse_hyperlinks(&escaped, 3);
    assert_eq!(targets, ["https://a.example/x", "file:///tmp/b.rs"]);
    // "go the docs ok": the link is on `the docs`, characters 3 to 10.
    assert_eq!(links[0].len(), 14);
    assert_eq!(links[0][2], None);
    assert!(links[0][3..11].iter().all(|link| *link == Some(0)));
    assert_eq!(links[0][11], None);
    assert!(links[1].iter().all(|link| *link == Some(1)));
    assert!(links[2].iter().all(|link| link.is_none()));
    // Without a link there is nothing to keep; with the wrong number of rows, nothing is trusted.
    assert_eq!(parse_hyperlinks("plain\n", 1), (Vec::new(), Vec::new()));
    assert_eq!(parse_hyperlinks(&escaped, 2), (Vec::new(), Vec::new()));
}

#[test]
fn words_that_are_an_address_cannot_point_to_another_site() {
    let esc = "\u{1b}";
    let view = |label: &str, target: &str| {
        let text = format!("go {label} now");
        let mut raw = capture(40, 1, &[(&text, false)]);
        raw.escaped = format!(
            "go {esc}]8;;{target}{esc}\\{label}{esc}]8;;{esc}\\ now{:<pad$}\n",
            "",
            pad = 40 - text.chars().count()
        );
        PaneView::parse(&raw).unwrap()
    };
    let nowhere = Path::new("/x");
    let click = |label: &str, target: &str| view(label, target).link_at(0, 6, &bases(nowhere));
    // Plain words go where they point.
    assert_eq!(
        click("the docs", "https://example.com/docs"),
        Some(Link::Url("https://example.com/docs".into()))
    );
    // An address on the same site may be longer than the words show.
    assert_eq!(
        click("https://example.com/a", "https://example.com/a/b"),
        Some(Link::Url("https://example.com/a/b".into()))
    );
    // An address that points elsewhere is what it says.
    assert_eq!(
        click("https://github.com/x", "https://evil.example/y"),
        Some(Link::Url("https://github.com/x".into()))
    );
    assert_eq!(
        url_host("https://User@Example.COM:8080/a?b#c").as_deref(),
        Some("example.com:8080")
    );
    assert_eq!(url_host("mailto:me@example.com"), None);
}

#[test]
fn a_double_width_character_that_went_to_the_next_row_does_not_break_the_join() {
    // 39 letters and a double-width character on a 40-column screen: the character did not fit
    // in the last column, so it is the first thing on the next row, and the cell it left behind
    // is not in the joined capture.
    let first = format!("{}a ", "a".repeat(38));
    let second = format!("{:<39}", "你 file");
    let raw = RawCapture {
        header: "40\t2\t0\t0\t\t0\t/work".into(),
        clients: String::new(),
        rows: format!("{first}\n{second}\n"),
        joined: format!("{}a你 file\n", "a".repeat(38)),
        escaped: String::new(),
    };
    let view = PaneView::parse(&raw).unwrap();
    assert_eq!(view.wraps, [true, false]);
    assert_eq!(view.padded, [true, false]);
    // The letters before it are a token that ends where the wide character begins.
    let located = view.locate(0, 5).unwrap();
    let text: String = located.chars.iter().collect();
    assert_eq!(text, format!("{}a你 file", "a".repeat(38)));
}

// ---- the screen -------------------------------------------------------------------------

/// A pane `cols` wide with `rows` rows and no history, showing `screen`; each entry is one row.
/// The capture is built the way tmux prints it: the rows padded to the full width, and the same
/// with wrapped rows joined (a row ends a line unless the next entry continues it).
fn capture(cols: usize, rows: usize, screen: &[(&str, bool)]) -> RawCapture {
    assert_eq!(screen.len(), rows);
    let padded: String = screen
        .iter()
        .map(|(text, _)| format!("{text:<cols$}\n"))
        .collect();
    let mut joined = String::new();
    for (text, wraps) in screen {
        if *wraps {
            joined.push_str(&format!("{text:<cols$}"));
        } else {
            // The unwritten tail of a line is not printed.
            joined.push_str(text);
            joined.push('\n');
        }
    }
    RawCapture {
        header: format!("{cols}\t{rows}\t0\t0\t\t0\t/work"),
        clients: String::new(),
        rows: padded,
        joined,
        escaped: String::new(),
    }
}

fn bases(cwd: &Path) -> Bases<'_> {
    Bases {
        cwd,
        root: None,
        home: None,
    }
}

#[test]
fn a_link_wrapped_over_rows_is_joined() {
    let raw = capture(
        10,
        4,
        &[
            ("go https:/", true),
            ("/ex.com/a/", true),
            ("b ok", false),
            ("", false),
        ],
    );
    assert_eq!(raw.joined, "go https://ex.com/a/b ok\n\n");
    let view = PaneView::parse(&raw).unwrap();
    let nowhere = Path::new("/nowhere");
    let url = Some(Link::Url("https://ex.com/a/b".into()));
    // From the first row, the middle one, and the last.
    assert_eq!(view.link_at(0, 5, &bases(nowhere)), url);
    assert_eq!(view.link_at(1, 3, &bases(nowhere)), url);
    assert_eq!(view.link_at(2, 0, &bases(nowhere)), url);
    // The words around it are not it.
    assert_eq!(view.link_at(0, 0, &bases(nowhere)), None);
    assert_eq!(view.link_at(2, 3, &bases(nowhere)), None);
    // Nor are blank cells, or a row past the screen.
    assert_eq!(view.link_at(2, 8, &bases(nowhere)), None);
    assert_eq!(view.link_at(9, 0, &bases(nowhere)), None);
}

// ---- what a link covers ------------------------------------------------------------------

/// The runs of cells the link under a cell covers, as (row, first column, end column).
fn runs_at(view: &PaneView, row: u32, col: u32, cwd: &Path) -> Option<Vec<(u32, u32, u32)>> {
    view.link_span_at(row, col, &bases(cwd)).map(|(_, runs)| {
        runs.into_iter()
            .map(|run| (run.row, run.cols.start, run.cols.end))
            .collect()
    })
}

#[test]
fn a_file_link_covers_its_position_too() {
    let tree = Tree::new();
    let raw = capture(30, 1, &[("see src/main.rs:12:3, then", false)]);
    let view = PaneView::parse(&raw).unwrap();
    let (link, runs) = view
        .link_span_at(0, 6, &bases(&tree.join("work")))
        .expect("a link");
    assert_eq!(
        Some(link),
        found(
            fs::canonicalize(tree.join("work/src/main.rs")).unwrap(),
            Some(12),
            Some(3),
            false
        )
    );
    // `src/main.rs:12:3` is columns 4 to 19, the comma after it is not.
    assert_eq!(
        runs,
        [LinkRow {
            row: 0,
            cols: 4..20
        }]
    );
}

#[test]
fn a_hyperlink_covers_the_words_it_is_on() {
    let esc = "\u{1b}";
    let mut raw = capture(30, 2, &[("see the docs here", false), ("", false)]);
    raw.escaped = format!(
        "see {esc}]8;;https://a.example/x{esc}\\the docs{esc}]8;;{esc}\\ here{:<13}\n{:<30}\n",
        "", ""
    );
    let view = PaneView::parse(&raw).unwrap();
    let nowhere = Path::new("/x");
    // `the docs` is columns 4 to 11, from either end of it.
    assert_eq!(runs_at(&view, 0, 4, nowhere), Some(vec![(0, 4, 12)]));
    assert_eq!(runs_at(&view, 0, 11, nowhere), Some(vec![(0, 4, 12)]));
}

#[test]
fn a_wide_character_covers_two_cells_and_a_row_out_of_view_none() {
    // A link that wrapped from the context above the screen: only its visible row is covered.
    let raw = RawCapture {
        header: "10\t1\t1\t0\t\t0\t/work".into(),
        clients: String::new(),
        rows: "go https:/\n/x.com/你 \n".into(),
        joined: "go https://x.com/你\n".into(),
        escaped: String::new(),
    };
    let view = PaneView::parse(&raw).unwrap();
    let nowhere = Path::new("/nowhere");
    assert_eq!(
        view.link_at(0, 2, &bases(nowhere)),
        Some(Link::Url("https://x.com/你".into()))
    );
    assert_eq!(runs_at(&view, 0, 2, nowhere), Some(vec![(0, 0, 9)]));
}

#[test]
fn rows_that_were_not_wrapped_are_not_joined() {
    // Two rows that both end at the edge, as an agent that wraps its own text leaves them.
    let raw = capture(
        10,
        3,
        &[("see aaaaaa", false), ("bbbbbb.rs", false), ("", false)],
    );
    let view = PaneView::parse(&raw).unwrap();
    assert_eq!(view.wraps, [false, false, false]);
    let tree = Tree::new();
    fs::write(tree.join("aaaaaabbbbbb.rs"), "").unwrap();
    fs::write(tree.join("bbbbbb.rs"), "").unwrap();
    assert_eq!(
        view.link_at(1, 2, &bases(&tree.0)),
        found(
            fs::canonicalize(tree.join("bbbbbb.rs")).unwrap(),
            None,
            None,
            false
        )
    );
}

#[test]
fn what_cannot_be_matched_up_is_not_joined() {
    // The joined capture disagrees with the rows: better no wrapping than a guess.
    let mut raw = capture(10, 2, &[("go https:/", true), ("/ex.com/a ", false)]);
    raw.joined = "something else entirely\n".into();
    let view = PaneView::parse(&raw).unwrap();
    assert_eq!(view.wraps, [false, false]);
    assert_eq!(view.link_at(0, 5, &bases(Path::new("/x"))), None);
}

#[test]
fn a_link_touching_the_edge_of_the_capture_may_be_cut() {
    // 4 rows, scrolled back 13 lines: the view is lines -13 to -10, and the capture ends 12 lines
    // below the view, at line 2, short of the pane's last line (3). A link that runs from the
    // view down to that last captured row may go on beyond it.
    let chain = |last: &str| {
        let mut rows = vec!["https://ex".to_owned()];
        rows.extend((0..14).map(|_| "aaaaaaaaaa".to_owned()));
        rows.push(last.to_owned());
        let wrapped: String = rows.iter().map(|row| format!("{row:<10}")).collect();
        RawCapture {
            header: "10\t4\t30\t13\tcopy-mode\t0\t/work".into(),
            clients: String::new(),
            // Lines -25 to -14 are other text; the chain is lines -13 to 2.
            rows: (-25..-13)
                .map(|_| format!("{:<10}\n", "x"))
                .chain(rows.iter().map(|row| format!("{row:<10}\n")))
                .collect(),
            joined: (-25..-13)
                .map(|_| "x\n".to_owned())
                .chain([format!("{}\n", wrapped.trim_end())])
                .collect(),
            escaped: String::new(),
        }
    };
    let nowhere = Path::new("/x");
    // The last captured row is full: the chain may continue below it, so the address is not
    // trusted, wherever on the chain the pointer is.
    let view = PaneView::parse(&chain("bbbbbbbbbb")).unwrap();
    assert!(view.end_cut);
    assert_eq!(view.link_at(0, 3, &bases(nowhere)), None);
    // The last row ends before the edge: the chain is whole.
    let view = PaneView::parse(&chain("bbbb")).unwrap();
    assert!(!view.end_cut);
    let address = format!("https://ex{}{}", "a".repeat(140), "bbbb");
    assert_eq!(
        view.link_at(0, 3, &bases(nowhere)),
        Some(Link::Url(address))
    );
}

#[test]
fn scrolled_back_the_view_is_offset_into_the_capture() {
    // 4 rows, 20 lines of history, scrolled back 3: the view shows lines -3 to 0. The capture
    // goes 12 lines further up (to -15) and as far down as the pane (3).
    let lines: Vec<String> = (-15..=3)
        .map(|line| format!("https://l{}.io", line + 100))
        .collect();
    let raw = RawCapture {
        header: "20\t4\t20\t3\tcopy-mode\t0\t/work".into(),
        clients: String::new(),
        rows: lines.iter().map(|line| format!("{line:<20}\n")).collect(),
        joined: lines.iter().map(|line| format!("{line}\n")).collect(),
        escaped: String::new(),
    };
    let view = PaneView::parse(&raw).unwrap();
    assert_eq!(view.mode, PaneMode::Scrolled);
    assert_eq!(view.scroll, 3);
    let top = view.link_at(0, 8, &bases(Path::new("/x")));
    assert_eq!(top, Some(Link::Url("https://l97.io".into())));
    let bottom = view.link_at(3, 8, &bases(Path::new("/x")));
    assert_eq!(bottom, Some(Link::Url("https://l100.io".into())));
}

#[test]
fn history_is_clamped_where_it_starts() {
    // Only 2 lines of history, so the capture starts at line -2, not -12.
    let lines = ["a", "b", "c", "d", "e", "f"];
    let raw = RawCapture {
        header: "10\t4\t2\t0\t\t0\t/work".into(),
        clients: String::new(),
        rows: lines.iter().map(|line| format!("{line:<10}\n")).collect(),
        joined: lines.iter().map(|line| format!("{line}\n")).collect(),
        escaped: String::new(),
    };
    // 6 rows are expected: lines -2 to 3.
    let view = PaneView::parse(&raw).unwrap();
    assert!(view.at_history_top);
    // Sizes that do not add up are an error, not a silent shift of the whole view.
    let short = RawCapture {
        rows: raw
            .rows
            .lines()
            .take(5)
            .map(|row| format!("{row}\n"))
            .collect(),
        ..raw
    };
    assert!(PaneView::parse(&short).is_err());
}

#[test]
fn the_desktop_client_gives_the_grid() {
    let raw = |clients: &str| RawCapture {
        clients: clients.to_owned(),
        ..capture(8, 2, &[("", false), ("", false)])
    };
    // No client: the pane.
    let view = PaneView::parse(&raw("")).unwrap();
    assert_eq!((view.cols, view.rows), (8, 2));
    // A control-mode client (the phone's, a watcher) never sizes the grid; of the others, the
    // last used does.
    let view = PaneView::parse(&raw(
        "1\t0\t30\t20\t900\n0\t0\t100\t40\t100\n0\t0\t120\t50\t200\n",
    ))
    .unwrap();
    assert_eq!((view.cols, view.rows), (120, 50));
    let view = PaneView::parse(&raw("1\t0\t30\t20\t900\n")).unwrap();
    assert_eq!((view.cols, view.rows), (8, 2));
    // Garbled lines are skipped.
    let view = PaneView::parse(&raw("0\t0\twide\t40\t100\nnonsense\n")).unwrap();
    assert_eq!((view.cols, view.rows), (8, 2));
}

#[test]
fn a_pane_in_another_mode_has_no_links() {
    let mut raw = capture(20, 1, &[("https://example.com", false)]);
    raw.header = "20\t1\t0\t0\ttree-mode\t0\t/work".into();
    let view = PaneView::parse(&raw).unwrap();
    assert_eq!(view.mode, PaneMode::Other);
    assert_eq!(view.link_at(0, 5, &bases(Path::new("/x"))), None);
    raw.header = "20\t1\t0\t0\t\t1\t/work".into();
    let view = PaneView::parse(&raw).unwrap();
    assert!(view.alternate);
    assert_eq!(
        view.link_at(0, 5, &bases(Path::new("/x"))),
        Some(Link::Url("https://example.com".into()))
    );
}

#[test]
fn malformed_answers_are_errors() {
    let good = capture(10, 1, &[("x", false)]);
    for header in ["", "10\t1", "ten\t1\t0\t0\t\t0\t/", "0\t1\t0\t0\t\t0\t/"] {
        let raw = RawCapture {
            header: header.into(),
            ..good.clone()
        };
        assert!(PaneView::parse(&raw).is_err(), "{header:?}");
    }
    let view = PaneView::parse(&good).unwrap();
    assert_eq!(view.cwd, Path::new("/work"));
}

#[test]
fn wide_characters_take_two_cells_and_marks_none() {
    // "世界" covers cells 0-3: pointing at cell 2 is the second character, and the path
    // that follows starts at cell 5.
    let raw = capture(30, 1, &[("世界 file:///tmp/a.txt", false)]);
    let view = PaneView::parse(&raw).unwrap();
    let located = view.locate(0, 8).unwrap();
    assert_eq!(located.chars[located.index], 'e');
    assert_eq!(char_at_cell("世界 ab", 0), Some(0));
    assert_eq!(char_at_cell("世界 ab", 1), Some(0));
    assert_eq!(char_at_cell("世界 ab", 2), Some(1));
    assert_eq!(char_at_cell("世界 ab", 4), Some(2));
    assert_eq!(char_at_cell("世界 ab", 5), Some(3));
    assert_eq!(char_at_cell("世界 ab", 7), None);
    // A combining accent shares the cell before it.
    assert_eq!(char_at_cell("e\u{301}x", 1), Some(2));
    assert_eq!(char_width('a'), 1);
    assert_eq!(char_width('é'), 1);
    assert_eq!(char_width('世'), 2);
    assert_eq!(char_width('\u{301}'), 0);
    assert_eq!(char_width('✅'), 2);
    assert_eq!(char_width('│'), 1);
}

#[test]
fn only_web_and_mail_links_are_ever_opened() {
    for url in [
        "https://example.com/a?b=c",
        "http://localhost:3000",
        "HTTPS://EXAMPLE.COM",
        "mailto:me@example.com",
    ] {
        assert!(is_openable_url(url), "{url}");
    }
    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "ssh://host",
        "riwork://pair?v=2",
        "/Users/me/file.txt",
        "",
        "httpx://example.com",
    ] {
        assert!(!is_openable_url(url), "{url}");
    }
}
