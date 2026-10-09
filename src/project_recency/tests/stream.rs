//! The change stream: what it hears, what that does to the dates, and that a
//! machine where nothing happens scans nothing. The first half needs no stream:
//! events are made up and applied to scans of real files. The second half makes
//! real FSEvents streams over temporary directories.

use super::*;
use crate::project_recency::fsevents::{Event, flag};
use std::{os::unix::ffi::OsStrExt, time::SystemTime};

const MODIFIED: u32 = flag::ITEM_MODIFIED;
const CREATED: u32 = flag::ITEM_CREATED;
const REMOVED: u32 = flag::ITEM_REMOVED;
const RENAMED: u32 = flag::ITEM_RENAMED;
const TOUCHED: u32 = flag::ITEM_INODE_META_MOD;
const DIRECTORY: u32 = flag::ITEM_IS_DIR;
const XATTR: u32 = 0x8000;

fn scans() -> usize {
    SCANS.with(std::cell::Cell::get)
}

fn roots_of(state: &State) -> Vec<PathBuf> {
    wanted_roots(state)
        .into_iter()
        .flat_map(|(_, roots)| roots)
        .collect()
}

fn sink_for(roots: &[PathBuf]) -> changes::Sink {
    changes::Sink::new(changes::Matcher::new(
        roots
            .iter()
            .map(|root| (root.clone(), git_dirs_outside(root))),
    ))
}

fn hear(sink: &changes::Sink, path: &Path, flags: u32) {
    sink.record([Event {
        path: path.as_os_str().as_bytes(),
        flags,
    }]);
}

/// Scans of real roots that the events made up by a test are applied to.
struct Heard {
    cache: Cache,
    sink: changes::Sink,
    wanted: Wanted,
}

impl Heard {
    /// Every root scanned, as a stream would have them, and nothing heard yet.
    fn new(state: &State) -> Self {
        settle();
        let wanted = wanted_roots(state);
        let sink = sink_for(&roots_of(state));
        let cache = scan_roots(Cache::new(), &wanted, LIMITS, Some(sink.matcher()));
        let heard = Self {
            cache,
            sink,
            wanted,
        };
        for root in heard.cache.roots.keys() {
            assert!(!heard.outcome(root).due(root), "{root:?} starts out quiet");
        }
        heard
    }

    fn hear(&mut self, path: &Path, flags: u32) -> &mut Self {
        hear(&self.sink, path, flags);
        self
    }

    /// Fold what was heard into the dates.
    fn apply(&mut self) {
        if let Some(pending) = self.sink.take() {
            changes::apply(&mut self.cache, self.sink.matcher(), pending);
        }
    }

    fn outcome(&self, root: &Path) -> &Outcome {
        &self.cache.roots[root.canonicalize().unwrap().as_path()]
    }

    fn date(&self, project: &str) -> Option<u64> {
        latest_edits(&self.cache, &self.wanted)
            .get(project)
            .copied()
    }

    /// Scan again what is due, as the next window to ask would.
    fn rescan(&mut self) -> usize {
        let before = scans();
        self.cache = scan_roots(
            self.cache.clone(),
            &self.wanted,
            LIMITS,
            Some(self.sink.matcher()),
        );
        scans() - before
    }
}

/// A repository with `src/a.rs` (100) and `src/b.rs` (50), both tracked.
fn two_files() -> (Fixture, PathBuf, PathBuf) {
    let repo = repository();
    let a = repo.file("src/a.rs", 100);
    let b = repo.file("src/b.rs", 50);
    repo.git(&["add", "src"]);
    (repo, a, b)
}

fn one_root(repo: &Fixture) -> State {
    state_of(vec![project("project", &[repo])])
}

#[test]
fn the_stream_is_the_only_thing_that_says_a_root_may_wait() {
    let repo = repository();
    repo.file("a.rs", 100);
    settle();
    let wanted = wanted_roots(&one_root(&repo));
    let polled = scan_roots(Cache::new(), &wanted, LIMITS, None);
    let root = &roots_of(&one_root(&repo))[0];
    assert!(polled.roots[root].due(root));
    let sink = sink_for(std::slice::from_ref(root));
    let watched = scan_roots(Cache::new(), &wanted, LIMITS, Some(sink.matcher()));
    assert!(!watched.roots[root].due(root));
}

#[test]
fn an_edit_raises_the_date_with_no_scan() {
    let (repo, _, b) = two_files();
    let mut heard = Heard::new(&one_root(&repo));
    assert_eq!(heard.date("project"), Some(100));

    edit(&b, 300);
    heard.hear(&b, MODIFIED | TOUCHED).apply();
    assert_eq!(heard.date("project"), Some(300));
    let root = repo.0.canonicalize().unwrap();
    assert!(!heard.outcome(&root).due(&root));
    let before = (scans(), git_runs());
    assert_eq!(heard.rescan(), 0);
    assert_eq!((scans(), git_runs()), before);
}

#[test]
fn an_edit_older_than_the_newest_changes_nothing() {
    let (repo, a, b) = two_files();
    let mut heard = Heard::new(&one_root(&repo));
    edit(&b, 70);
    heard.hear(&b, MODIFIED).apply();
    assert_eq!(heard.date("project"), Some(100));
    assert!(!heard.outcome(&repo.0).dirty);
    // Another file with the newest date does not hold it either.
    edit(&b, 100);
    heard.hear(&b, MODIFIED).apply();
    assert!(!heard.outcome(&repo.0).dirty);
    fs::remove_file(&b).unwrap();
    heard.hear(&b, REMOVED).apply();
    assert!(!heard.outcome(&repo.0).dirty);
    assert_eq!(heard.date("project"), Some(100));
    assert!(a.exists());
}

#[test]
fn the_file_with_the_newest_date_going_back_or_away_is_looked_at_again() {
    for how in [
        "removed",
        "older",
        "replaced by a link",
        "directory in its place",
    ] {
        let (repo, a, _) = two_files();
        let mut heard = Heard::new(&one_root(&repo));
        assert_eq!(heard.date("project"), Some(100), "{how}");
        match how {
            "removed" => fs::remove_file(&a).unwrap(),
            "older" => edit(&a, 10),
            "replaced by a link" => {
                fs::remove_file(&a).unwrap();
                std::os::unix::fs::symlink(repo.0.join("src/b.rs"), &a).unwrap();
            }
            _ => {
                fs::remove_file(&a).unwrap();
                fs::create_dir(&a).unwrap();
            }
        }
        heard.hear(&a, MODIFIED | REMOVED | CREATED).apply();
        assert!(heard.outcome(&repo.0).dirty, "{how}");
        // The scan that follows lowers the date, and settles the root again.
        settle();
        assert_eq!(heard.rescan(), 1, "{how}");
        assert_eq!(heard.date("project"), Some(50), "{how}");
        assert!(!heard.outcome(&repo.0).dirty, "{how}");
        assert_eq!(heard.rescan(), 0, "{how}");
    }
}

#[test]
fn a_moved_directory_is_looked_at_again_when_it_held_the_newest_date() {
    let repo = Fixture::new();
    repo.file("pkg/deep/new.rs", 100);
    repo.file("other/old.rs", 40);
    let mut heard = Heard::new(&one_root(&repo));
    let pkg = repo.0.join("pkg");
    // A directory with the newest file in it, gone from where it was.
    fs::rename(&pkg, repo.0.join("moved")).unwrap();
    heard.hear(&pkg, RENAMED | DIRECTORY).apply();
    assert!(heard.outcome(&repo.0).dirty);
    settle();
    assert_eq!(heard.rescan(), 1);
    assert_eq!(heard.date("project"), Some(100));
    // One that held nothing of the kind may go without a scan.
    fs::remove_dir_all(repo.0.join("other")).unwrap();
    heard
        .hear(&repo.0.join("other"), REMOVED | DIRECTORY)
        .apply();
    assert!(!heard.outcome(&repo.0).dirty);
    // One that came in holds files nobody has listed.
    fs::create_dir(repo.0.join("arrived")).unwrap();
    heard
        .hear(&repo.0.join("arrived"), CREATED | DIRECTORY)
        .apply();
    assert!(heard.outcome(&repo.0).dirty);
}

#[test]
fn a_root_without_a_file_list_cannot_check_what_it_hears() {
    let (repo, _, b) = two_files();
    let mut heard = Heard::new(&one_root(&repo));
    heard.cache.listings.clear();
    edit(&b, 70);
    heard.hear(&b, MODIFIED).apply();
    assert!(heard.outcome(&repo.0).dirty);
}

#[test]
fn a_new_file_makes_git_list_again_and_a_changed_old_one_does_not() {
    let repo = repository();
    let ignore = repo.file(".gitignore", 10);
    fs::write(&ignore, "*.log\n").unwrap();
    set_time(&ignore, 10);
    repo.file("a.rs", 100);
    repo.file("build.log", 900);
    repo.git(&["add", ".gitignore", "a.rs"]);
    let mut heard = Heard::new(&one_root(&repo));
    let root = repo.0.canonicalize().unwrap();
    assert_eq!(heard.date("project"), Some(100));

    // The ignored file is written to: it was there, Git does not list it.
    let log = repo.0.join("build.log");
    edit(&log, 950);
    heard.hear(&log, MODIFIED).apply();
    assert!(!heard.outcome(&root).dirty);
    assert_eq!(heard.date("project"), Some(100));
    // A file of that name appears: it may be one Git lists, or one it ignores.
    heard.hear(&log, CREATED | MODIFIED).apply();
    assert!(heard.outcome(&root).dirty);
    assert!(!heard.cache.listings.contains_key(&root));
    settle();
    let (rescans, runs) = spawned(|| heard.rescan());
    assert_eq!((rescans, runs), (1, 1));
    assert_eq!(heard.date("project"), Some(100));

    // One Git does list.
    let new = repo.file("new.rs", 300);
    heard.hear(&new, CREATED | MODIFIED).apply();
    settle();
    assert_eq!(heard.rescan(), 1);
    assert_eq!(heard.date("project"), Some(300));
    // A file that came and went again leaves nothing to look at.
    let temporary = repo.file("swap.tmp", 400);
    fs::remove_file(&temporary).unwrap();
    heard.hear(&temporary, CREATED | MODIFIED | REMOVED).apply();
    assert!(!heard.outcome(&root).dirty);
}

#[test]
fn ignore_files_and_the_index_make_git_list_again() {
    let (repo, _, _) = two_files();
    let root = repo.0.canonicalize().unwrap();
    let mut heard = Heard::new(&one_root(&repo));
    let ignore = repo.0.join("src/.gitignore");
    for (path, flags) in [
        (ignore.clone(), MODIFIED),
        (repo.0.join(".gitignore"), CREATED),
    ] {
        heard.hear(&path, flags).apply();
        assert!(heard.outcome(&root).dirty, "{path:?}");
        assert!(!heard.cache.listings.contains_key(&root), "{path:?}");
        settle();
        heard.rescan();
        assert!(!heard.outcome(&root).dirty);
    }
    // The index and the rest of what Git reads keep the file list when the
    // fingerprint of it still holds.
    for name in ["index", "HEAD", "config", "info/exclude"] {
        heard
            .hear(&repo.0.join(".git").join(name), MODIFIED)
            .apply();
        assert!(heard.outcome(&root).dirty, "{name}");
        assert!(heard.cache.listings.contains_key(&root), "{name}");
        let (_, runs) = spawned(|| heard.rescan());
        assert_eq!(runs, 0, "{name}");
    }
}

#[test]
fn what_a_scan_never_looks_at_is_not_remembered_at_all() {
    let (repo, _, _) = two_files();
    let heard = Heard::new(&one_root(&repo));
    let root = &repo.0;
    for (path, flags) in [
        ("target/debug/build.rs", CREATED | MODIFIED),
        ("node_modules/a/b/c.js", MODIFIED),
        ("src/.DS_Store", MODIFIED),
        (".git/objects/ab/cdef", CREATED),
        (".git/refs/heads/main", MODIFIED),
        (".git/index.lock", CREATED),
        (".git/logs/HEAD", MODIFIED),
        // A repository in a directory no scan goes into.
        ("node_modules/package/.git/index", MODIFIED),
        ("target/checkout/.git/HEAD", MODIFIED),
        ("node_modules/package/.git", CREATED | DIRECTORY),
        // Only an attribute changed.
        ("src/a.rs", XATTR),
        ("src/a.rs", flag::ITEM_IS_DIR << 1 | XATTR),
        // The root itself.
        ("", MODIFIED),
    ] {
        hear(&heard.sink, &root.join(path), flags);
    }
    assert!(heard.sink.take().is_none());
}

#[test]
fn only_the_files_git_reads_make_the_index_matter() {
    let (repo, _, _) = two_files();
    let root = repo.0.canonicalize().unwrap();
    let sink = sink_for(std::slice::from_ref(&root));
    for name in [
        "objects/ab/cd",
        "refs/heads/x",
        "index.lock",
        "logs/HEAD",
        "FETCH_HEAD",
    ] {
        hear(&sink, &root.join(".git").join(name), MODIFIED);
    }
    assert!(sink.take().is_none());
    for name in [
        "index",
        "HEAD",
        "config",
        "info/exclude",
        "commondir",
        "config.worktree",
    ] {
        hear(&sink, &root.join(".git").join(name), MODIFIED);
        let pending = sink.take().expect(name);
        assert!(pending.rescan(0), "{name}");
    }
    // A repository that appears or disappears is a different set of files.
    hear(&sink, &root.join("nested/.git"), CREATED | DIRECTORY);
    assert!(sink.take().expect("created").relist(0));
    hear(&sink, &root.join("nested/.git"), MODIFIED);
    assert!(sink.take().is_none());
}

#[test]
fn a_failed_root_keeps_its_own_schedule_whatever_is_heard() {
    let (repo, _, _) = two_files();
    let mut heard = Heard::new(&one_root(&repo));
    let root = repo.0.canonicalize().unwrap();
    {
        let outcome = Arc::make_mut(heard.cache.roots.get_mut(&root).unwrap());
        outcome.result = Err(());
        outcome.failures = 1;
    }
    hear(&heard.sink, &root.join(".git/index"), MODIFIED);
    hear(&heard.sink, &root.join("src/new.rs"), CREATED);
    heard.apply();
    hear(&heard.sink, &root.join("src"), flag::MUST_SCAN_SUBDIRS);
    heard.apply();
    hear(&heard.sink, Path::new("/"), flag::EVENT_IDS_WRAPPED);
    heard.apply();
    // Not scanned sooner for any of it, and its file list is not trusted.
    assert!(!heard.outcome(&root).dirty);
    assert!(!heard.outcome(&root).due(&root));
    assert!(heard.cache.listings.is_empty());
}

#[test]
fn names_that_may_be_spelled_either_way_are_looked_at_again() {
    for same_spelling in [true, false] {
        let repo = repository();
        let accented = repo.file("src/caf\u{e9}.rs", 100);
        repo.file("src/b.rs", 50);
        repo.git(&["add", "src"]);
        let mut heard = Heard::new(&one_root(&repo));
        // The file system may give the accent as a letter, or as a letter and a
        // mark that goes over it. Git lists what it was told.
        let reported = if same_spelling {
            accented.clone()
        } else {
            repo.0.join("src/cafe\u{301}.rs")
        };
        edit(&accented, 150);
        heard.hear(&reported, MODIFIED).apply();
        assert_eq!(heard.outcome(&repo.0).dirty, !same_spelling);
        if same_spelling {
            assert_eq!(heard.date("project"), Some(150));
        }
        fs::remove_file(&accented).unwrap();
        heard.hear(&reported, REMOVED).apply();
        assert!(heard.outcome(&repo.0).dirty);
    }
}

#[test]
fn files_behind_a_directory_that_became_a_link_are_not_counted() {
    for holder in [false, true] {
        let (repo, a, b) = two_files();
        let outside = Fixture::new();
        outside.file("a.rs", 900);
        outside.file("b.rs", 950);
        let mut heard = Heard::new(&one_root(&repo));
        fs::remove_dir_all(repo.0.join("src")).unwrap();
        std::os::unix::fs::symlink(&outside.0, repo.0.join("src")).unwrap();
        // Heard before the directory was, while it was one.
        let file = if holder { &a } else { &b };
        heard.hear(file, MODIFIED).apply();
        assert_eq!(heard.date("project"), Some(100));
        assert_eq!(heard.outcome(&repo.0).dirty, holder);
    }
}

#[test]
fn lost_events_make_the_roots_they_may_have_been_in_scan_again() {
    let (one, _, _) = two_files();
    let (two, _, _) = two_files();
    let state = state_of(vec![project("one", &[&one]), project("two", &[&two])]);
    let roots = roots_of(&state);
    // Dropped for want of speed, merged into the directory that holds them all,
    // or with the ids that name them wrapped around.
    for (path, lost) in [
        ("/", flag::USER_DROPPED | flag::MUST_SCAN_SUBDIRS),
        ("/", flag::KERNEL_DROPPED | flag::MUST_SCAN_SUBDIRS),
        ("/", flag::KERNEL_DROPPED),
        ("/", flag::EVENT_IDS_WRAPPED),
        ("/nowhere/at/all", flag::EVENT_IDS_WRAPPED),
        ("/nowhere/at/all", flag::MUST_SCAN_SUBDIRS),
    ] {
        let mut heard = Heard::new(&state);
        hear(&heard.sink, Path::new(path), lost);
        heard.apply();
        for root in &roots {
            assert!(heard.outcome(root).dirty, "{path} {lost:#x}");
        }
        assert!(heard.cache.listings.is_empty());
        settle();
        assert_eq!(heard.rescan(), 2);
    }
}

#[test]
fn losing_events_where_nothing_counts_costs_no_scan() {
    let (one, _, _) = two_files();
    let (two, _, _) = two_files();
    let state = state_of(vec![project("one", &[&one]), project("two", &[&two])]);
    let (first, second) = (one.0.canonicalize().unwrap(), two.0.canonicalize().unwrap());
    let mut heard = Heard::new(&state);
    // A build that outruns the kernel loses events by the thousand.
    for (path, lost) in [
        (
            first.join("target/debug"),
            flag::MUST_SCAN_SUBDIRS | flag::KERNEL_DROPPED,
        ),
        (
            first.join("node_modules"),
            flag::MUST_SCAN_SUBDIRS | flag::USER_DROPPED,
        ),
        (first.join("src/.DS_Store"), flag::MUST_SCAN_SUBDIRS),
    ] {
        hear(&heard.sink, &path, lost);
    }
    heard.apply();
    assert!(!heard.outcome(&first).dirty);
    assert!(!heard.outcome(&second).dirty);
    // Where the files Git lists live, or its own, it does.
    hear(&heard.sink, &first.join("src"), flag::MUST_SCAN_SUBDIRS);
    heard.apply();
    assert!(heard.outcome(&first).dirty);
    assert!(!heard.outcome(&second).dirty);
    hear(
        &heard.sink,
        &second.join(".git/objects"),
        flag::MUST_SCAN_SUBDIRS,
    );
    heard.apply();
    assert!(heard.outcome(&second).dirty);
}

#[test]
fn a_directory_that_must_be_scanned_makes_the_roots_in_it_scan_again() {
    let outer = Fixture::new();
    outer.file("note.txt", 5);
    let inner = Fixture(outer.0.join("inner"));
    inner.file("a.rs", 100);
    let (apart, _, _) = two_files();
    let state = state_of(vec![
        project("wide", &[&outer]),
        project("narrow", &[&inner]),
        project("apart", &[&apart]),
    ]);
    let wide = outer.0.canonicalize().unwrap();
    let narrow = inner.0.canonicalize().unwrap();
    let separate = apart.0.canonicalize().unwrap();
    let mut heard = Heard::new(&state);

    // Below the inner root: the outer one holds it, so both are in question.
    hear(&heard.sink, &narrow.join("deep"), flag::MUST_SCAN_SUBDIRS);
    heard.apply();
    assert!(heard.outcome(&wide).dirty);
    assert!(heard.outcome(&narrow).dirty);
    assert!(!heard.outcome(&separate).dirty);
    settle();
    assert_eq!(heard.rescan(), 2);
    // A root that moved or was unmounted is no longer what it was.
    for lost in [flag::ROOT_CHANGED, flag::MOUNT, flag::UNMOUNT] {
        hear(&heard.sink, &separate, lost);
        heard.apply();
        assert!(heard.outcome(&separate).dirty, "{lost:#x}");
        assert!(!heard.outcome(&wide).dirty, "{lost:#x}");
        settle();
        assert_eq!(heard.rescan(), 1);
    }
    // Merged events of a place that none of the roots are in say nothing about
    // them, but not knowing where they are is not knowing anything.
    hear(
        &heard.sink,
        Path::new("/nowhere/at/all"),
        flag::MUST_SCAN_SUBDIRS,
    );
    heard.apply();
    for root in [&wide, &narrow, &separate] {
        assert!(heard.outcome(root).dirty);
    }
}

#[test]
fn following_too_many_changes_is_worse_than_scanning_again() {
    let repo = Fixture::new();
    repo.file("a.rs", 100);
    let heard = Heard::new(&one_root(&repo));
    let root = repo.0.canonicalize().unwrap();
    for number in 0..=changes::MAX_PENDING {
        hear(
            &heard.sink,
            &root.join(format!("generated/{number}.rs")),
            MODIFIED,
        );
    }
    let pending = heard.sink.take().unwrap();
    assert!(pending.relist(0));
    assert_eq!(pending.files(0), 0);
}

#[test]
fn a_root_inside_a_root_hears_what_happens_in_it_for_both() {
    let outer = Fixture::new();
    outer.file("a.txt", 100);
    let inner = Fixture(outer.0.join("inner"));
    let file = inner.file("b.txt", 200);
    let state = state_of(vec![
        project("wide", &[&outer]),
        project("narrow", &[&inner]),
    ]);
    let mut heard = Heard::new(&state);
    edit(&file, 400);
    heard.hear(&file, MODIFIED).apply();
    assert_eq!(heard.date("wide"), Some(400));
    assert_eq!(heard.date("narrow"), Some(400));
    // The outer one only counts the file once its own walk would have.
    let wide = outer.0.canonicalize().unwrap();
    assert!(heard.outcome(&wide).holder(&wide.join("inner/b.txt")));
}

#[test]
fn a_repository_inside_a_folder_decides_for_the_files_in_it() {
    let folder = Fixture::new();
    folder.file("notes.txt", 10);
    let nested = folder.0.join("repository");
    fs::create_dir(&nested).unwrap();
    git_in(&nested, &["init", "--quiet"]);
    let ignore = folder.file("repository/.gitignore", 20);
    fs::write(&ignore, "generated/\n").unwrap();
    set_time(&ignore, 20);
    folder.file("repository/source.rs", 30);
    folder.file("repository/generated/output.rs", 900);
    let mut heard = Heard::new(&one_root(&folder));
    let root = folder.0.canonicalize().unwrap();
    assert_eq!(heard.date("project"), Some(30));

    // Ignored by the repository, so no edit to it counts.
    let generated = nested.join("generated/output.rs");
    edit(&generated, 950);
    heard.hear(&generated, MODIFIED).apply();
    assert_eq!(heard.date("project"), Some(30));
    assert!(!heard.outcome(&root).dirty);
    // The folder's own files and the repository's counted ones do.
    let notes = folder.0.join("notes.txt");
    edit(&notes, 60);
    heard.hear(&notes, MODIFIED).apply();
    assert_eq!(heard.date("project"), Some(60));
    let source = nested.join("source.rs");
    edit(&source, 80);
    heard.hear(&source, MODIFIED).apply();
    assert_eq!(heard.date("project"), Some(80));
    // And a file new to the repository sends the repository to Git again.
    let added = folder.file("repository/added.rs", 99);
    heard.hear(&added, CREATED).apply();
    assert!(heard.outcome(&root).dirty);
    assert!(
        !heard
            .cache
            .listings
            .contains_key(&nested.canonicalize().unwrap())
    );
}

#[test]
fn a_linked_worktree_hears_its_git_directory_outside_the_root() {
    let repository = repository();
    repository.file("a.rs", 100);
    repository.git(&["add", "a.rs"]);
    repository.git(&[
        "-c",
        "user.name=Recency Test",
        "-c",
        "user.email=recency@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "source",
    ]);
    let holder = Fixture::new();
    let worktree = holder.0.join("worktree");
    repository.git(&[
        "worktree",
        "add",
        "--quiet",
        "-b",
        "linked",
        worktree.to_str().unwrap(),
    ]);
    let root = worktree.canonicalize().unwrap();
    let outside = git_dirs_outside(&root);
    let common = repository.0.canonicalize().unwrap().join(".git");
    assert_eq!(outside.len(), 2, "{outside:?}");
    assert!(outside.contains(&common));
    assert!(outside.iter().all(|dir| dir.starts_with(&common)));

    let sink = sink_for(std::slice::from_ref(&root));
    // Only the stream's paths matter: the worktree, and the Git directories
    // outside it, which here are one tree.
    assert_eq!(sink.matcher().paths().len(), 2);
    let own = outside.iter().find(|dir| **dir != common).unwrap();
    hear(&sink, &own.join("index"), MODIFIED);
    assert!(sink.take().expect("worktree index").rescan(0));
    hear(&sink, &common.join("info/exclude"), MODIFIED);
    assert!(sink.take().expect("shared exclude").rescan(0));
    hear(&sink, &common.join("config"), MODIFIED);
    assert!(sink.take().expect("shared configuration").rescan(0));
    // The index and HEAD of the repository itself are the main worktree's.
    hear(&sink, &common.join("index"), MODIFIED);
    hear(&sink, &common.join("HEAD"), MODIFIED);
    hear(&sink, &common.join("objects/ab/cd"), CREATED);
    hear(&sink, &common.join("refs/heads/linked"), MODIFIED);
    assert!(sink.take().is_none());
}

#[test]
#[ignore = "slow: real FSEvents stream; waits for the system to report changes"]
fn a_stream_holds_its_listener_once_and_lets_it_go() {
    let dir = Fixture::new();
    let sink = Arc::new(sink_for(std::slice::from_ref(&dir.0)));
    let stream = fsevents::Stream::start(&sink.matcher().paths(), LATENCY_UNDER_TEST, &sink)
        .expect("a stream");
    assert_eq!(Arc::strong_count(&sink), 2);
    drop(stream);
    // The system lets go of it a moment after the stream is released.
    let started = Instant::now();
    while Arc::strong_count(&sink) > 1 {
        assert!(started.elapsed() < HEARING, "the listener is still held");
        thread::sleep(Duration::from_millis(10));
    }
    // A path it cannot take is refused, and nothing is kept.
    let odd = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff\xfe"));
    assert!(fsevents::Stream::start(&[odd], LATENCY_UNDER_TEST, &sink).is_none());
    assert_eq!(Arc::strong_count(&sink), 1);
}

#[test]
fn the_stream_covers_each_tree_once() {
    let outer = Fixture::new();
    outer.file("a.txt", 1);
    let inner = Fixture(outer.0.join("inner"));
    inner.file("b.txt", 2);
    let apart = Fixture::new();
    apart.file("c.txt", 3);
    let sink = sink_for(&roots_of(&state_of(vec![
        project("a", &[&outer, &inner]),
        project("b", &[&apart]),
    ])));
    let paths = sink.matcher().paths();
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&outer.0.canonicalize().unwrap().as_path()));
    assert!(paths.contains(&apart.0.canonicalize().unwrap().as_path()));
    assert!(sink.matcher().watches(&inner.0.canonicalize().unwrap()));
}

#[test]
fn a_file_list_can_tell_what_it_lists_in_any_order() {
    let files = Files {
        paths: b"z/last.rs\0a.rs\0m/n/o.rs\0b.rs\0".to_vec(),
        entries: 4,
        sorted: OnceLock::new(),
    };
    for listed in ["z/last.rs", "a.rs", "m/n/o.rs", "b.rs"] {
        assert!(files.contains(listed.as_bytes()), "{listed}");
    }
    for unlisted in [
        "",
        "a",
        "a.rs/",
        "m",
        "m/n",
        "c.rs",
        "z/last.rs0",
        "last.rs",
    ] {
        assert!(!files.contains(unlisted.as_bytes()), "{unlisted}");
    }
}

#[test]
fn a_root_comes_due_for_the_safety_net_at_a_time_of_its_own() {
    let (repo, _, _) = two_files();
    let heard = Heard::new(&one_root(&repo));
    let root = repo.0.canonicalize().unwrap();
    let mut outcome = heard.outcome(&root).clone();
    let aged = ago;

    outcome.at = aged(SAFETY_NET - Duration::from_secs(60));
    assert!(!outcome.due(&root));
    outcome.at = aged(SAFETY_NET + SAFETY_SPREAD);
    assert!(outcome.due(&root));
    // Roots do not all come due at once.
    let times: BTreeSet<_> = (0..50)
        .map(|index| safety_net(Path::new(&format!("/projects/root-{index}"))))
        .collect();
    assert!(times.len() > 25);
    assert!(
        times
            .iter()
            .all(|time| { *time >= SAFETY_NET && *time < SAFETY_NET + SAFETY_SPREAD })
    );
    // A failed root is tried again after a period, then after longer.
    let mut failed = outcome.clone();
    failed.result = Err(());
    for (failures, wait) in [
        (1, 30),
        (2, 60),
        (3, 120),
        (4, 240),
        (5, 480),
        (6, 600),
        (50, 600),
    ] {
        failed.failures = failures;
        assert_eq!(retry_after(failures), Duration::from_secs(wait));
        failed.at = aged(Duration::from_secs(wait - 5));
        assert!(!failed.due(&root), "{failures}");
        failed.at = aged(Duration::from_secs(wait + 5));
        assert!(failed.due(&root), "{failures}");
    }
    // Nothing is waited for without a stream.
    let mut polled = outcome;
    polled.at = Instant::now();
    polled.watched = false;
    assert!(polled.due(&root));
}

#[test]
fn a_root_whose_file_list_was_not_kept_is_scanned_again_at_once() {
    let repo = repository();
    repo.file("a.rs", 100);
    repo.git(&["add", "a.rs"]);
    // An index dated in the future, like one just written, cannot be told from
    // one that changes again, so a listing taken now is not remembered.
    let index = repo.0.join(".git/index");
    File::open(&index)
        .unwrap()
        .set_times(FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(3600)))
        .unwrap();
    let state = one_root(&repo);
    let wanted = wanted_roots(&state);
    let sink = sink_for(&roots_of(&state));
    let hurried = scan_roots(Cache::new(), &wanted, LIMITS, Some(sink.matcher()));
    let root = &roots_of(&state)[0];
    assert!(hurried.listings.is_empty());
    assert!(hurried.roots[root].dirty);
    assert!(hurried.roots[root].due(root));
    set_time(&index, 1000);
    settle();
    let calm = scan_roots(hurried, &wanted, LIMITS, Some(sink.matcher()));
    assert!(!calm.roots[root].dirty);
    assert!(!calm.roots[root].due(root));
}

#[test]
fn volumes_that_do_not_report_changes_are_polled() {
    let repo = Fixture::new();
    assert!(local_volume(&repo.0));
    assert!(!local_volume(&repo.0.join("missing")));
    assert!(reports_changes(
        libc::MNT_LOCAL as u32 | libc::MNT_RDONLY as u32
    ));
    assert!(!reports_changes(libc::MNT_RDONLY as u32));
    assert!(!reports_changes(0));
}

// What follows makes real streams.

/// How long a test waits for the system to say what happened. FSEvents answers
/// within the latency of the stream, a few tenths of a second, but a machine
/// under load can be a long way behind.
const HEARING: Duration = Duration::from_secs(60);
const LATENCY_UNDER_TEST: Duration = Duration::from_millis(100);

fn watching() -> Shared {
    Shared::watching(LATENCY_UNDER_TEST)
}

/// Ask as a window would until the project has `date`.
fn wait_for(shared: &Shared, state: &State, project: &str, date: u64) -> bool {
    let started = Instant::now();
    loop {
        if shared.scan(state, ALWAYS).get(project) == Some(&date) {
            return true;
        }
        if started.elapsed() > HEARING {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// The instant `by` ago.
fn ago(by: Duration) -> Instant {
    Instant::now()
        .checked_sub(by)
        .expect("the machine has been up for longer than the age")
}

/// Ask until a second passes with no scan: whatever the system still had to say
/// about the fixture, made before the stream was, has been heard and dealt with.
fn quiet(shared: &Shared, state: &State) {
    let (mut last, mut since, began) = (scans(), Instant::now(), Instant::now());
    while since.elapsed() < Duration::from_millis(1200) {
        assert!(began.elapsed() < HEARING, "the scans never stopped");
        thread::sleep(Duration::from_millis(100));
        shared.scan(state, ALWAYS);
        if scans() != last {
            last = scans();
            since = Instant::now();
        }
    }
}

/// A cache over `state` with its first scan done and the stream quiet.
fn started(state: &State) -> Shared {
    settle();
    let shared = watching();
    shared.scan(state, ALWAYS);
    quiet(&shared, state);
    shared
}

/// Whether the listener of a stream that was stopped is let go. The system
/// frees it a moment after the stream is released, on a queue of its own.
fn let_go(sink: &std::sync::Weak<changes::Sink>) -> bool {
    let started = Instant::now();
    while sink.upgrade().is_some() {
        if started.elapsed() > Duration::from_secs(20) {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

fn watched_roots(shared: &Shared) -> Vec<PathBuf> {
    shared
        .lock()
        .sink()
        .map_or_else(Vec::new, |sink| sink.matcher().roots().to_vec())
}

#[test]
#[ignore = "slow: real FSEvents stream; waits for the system to report changes"]
fn an_edit_to_a_tracked_file_is_heard_with_no_scan_and_no_git() {
    let (repo, a, _) = two_files();
    let state = one_root(&repo);
    settle();
    let shared = watching();
    let (first, runs) = spawned(|| shared.scan(&state, ALWAYS));
    assert_eq!(first.get("project"), Some(&100));
    assert_eq!(runs, 1);
    quiet(&shared, &state);

    let (scanned, git) = (scans(), git_runs());
    edit(&a, 250);
    assert!(wait_for(&shared, &state, "project", 250));
    // Quiet again: the answer comes from the cache however often it is asked.
    for _ in 0..20 {
        assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&250));
    }
    assert_eq!(scans() - scanned, 0, "scanned a root");
    assert_eq!(git_runs() - git, 0, "ran Git");
}

#[test]
#[ignore = "slow: real FSEvents stream; waits for the system to report changes"]
fn a_machine_where_nothing_happens_scans_nothing() {
    let (repo, _, _) = two_files();
    let other = Fixture::new();
    other.file("notes.txt", 7);
    let state = state_of(vec![project("project", &[&repo, &other])]);
    let shared = started(&state);
    let (scanned, git) = (scans(), git_runs());
    let started_at = Instant::now();
    let mut asks = 0;
    while started_at.elapsed() < Duration::from_secs(4) {
        assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&100));
        asks += 1;
        thread::sleep(Duration::from_millis(100));
    }
    assert!(asks > 20);
    assert_eq!((scans() - scanned, git_runs() - git), (0, 0));
}

#[test]
#[ignore = "slow: real FSEvents stream; waits for the system to report changes"]
fn changed_ignore_rules_and_index_are_heard() {
    let repo = repository();
    let ignore = repo.file(".gitignore", 10);
    fs::write(&ignore, "*.log\n").unwrap();
    set_time(&ignore, 10);
    repo.file("a.rs", 100);
    repo.file("secret.rs", 800);
    repo.file("build.log", 900);
    repo.git(&["add", ".gitignore", "a.rs"]);
    let state = one_root(&repo);
    let shared = started(&state);
    assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&800));

    // An ignore rule.
    fs::write(&ignore, "*.log\nsecret.rs\n").unwrap();
    set_time(&ignore, 20);
    assert!(wait_for(&shared, &state, "project", 100));
    quiet(&shared, &state);
    // Forcing an ignored file into the index changes no directory at all.
    let (scanned, git) = (scans(), git_runs());
    repo.git(&["add", "-f", "build.log"]);
    assert!(wait_for(&shared, &state, "project", 900));
    quiet(&shared, &state);
    assert_eq!((scans() - scanned, git_runs() - git), (1, 1));
}

#[test]
#[ignore = "slow: real FSEvents stream; waits for the system to report changes"]
fn a_linked_worktree_hears_its_index_outside_the_root() {
    let repository = repository();
    let ignore = repository.file(".gitignore", 10);
    fs::write(&ignore, "*.log\n").unwrap();
    set_time(&ignore, 10);
    repository.file("a.rs", 100);
    repository.git(&["add", ".gitignore", "a.rs"]);
    repository.git(&[
        "-c",
        "user.name=Recency Test",
        "-c",
        "user.email=recency@example.invalid",
        "commit",
        "--quiet",
        "-m",
        "source",
    ]);
    let holder = Fixture::new();
    let worktree = holder.0.join("worktree");
    repository.git(&[
        "worktree",
        "add",
        "--quiet",
        "-b",
        "linked",
        worktree.to_str().unwrap(),
    ]);
    let linked = Fixture(worktree.clone());
    set_time(&worktree.join("a.rs"), 100);
    set_time(&worktree.join(".gitignore"), 10);
    linked.file("build.log", 900);
    let state = state_of(vec![project("project", &[&linked])]);
    let shared = started(&state);
    assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&100));

    let (scanned, git) = (scans(), git_runs());
    git_in(&worktree, &["add", "-f", "build.log"]);
    assert!(wait_for(&shared, &state, "project", 900));
    quiet(&shared, &state);
    assert_eq!((scans() - scanned, git_runs() - git), (1, 1));
}

#[test]
#[ignore = "slow: real FSEvents stream; waits for the system to report changes"]
fn windows_asking_while_files_change_end_up_with_the_same_date() {
    let repo = repository();
    let files: Vec<_> = (0..20)
        .map(|number| repo.file(&format!("src/f{number}.rs"), 100))
        .collect();
    repo.git(&["add", "src"]);
    let state = one_root(&repo);
    let shared = started(&state);
    let writing = std::sync::atomic::AtomicBool::new(true);
    thread::scope(|scope| {
        let windows: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let mut previous = 0;
                    while writing.load(std::sync::atomic::Ordering::Relaxed) {
                        let date = *shared.scan(&state, ALWAYS).get("project").unwrap();
                        // Edits only ever raise the date here.
                        assert!(date >= previous);
                        previous = date;
                        thread::sleep(Duration::from_millis(5));
                    }
                })
            })
            .collect();
        for number in 0..100 {
            // Only the date, so there is no moment when the file is dated now.
            set_time(&files[number % files.len()], 1000 + number as u64);
            thread::sleep(Duration::from_millis(15));
        }
        writing.store(false, std::sync::atomic::Ordering::Relaxed);
        for window in windows {
            window.join().unwrap();
        }
    });
    assert!(wait_for(&shared, &state, "project", 1099));
}

#[test]
fn without_a_stream_every_ask_scans_as_before() {
    let (repo, a, _) = two_files();
    let state = one_root(&repo);
    settle();
    let shared = Shared::with(Source::Unavailable);
    assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&100));
    let scanned = scans();
    edit(&a, 250);
    assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&250));
    assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&250));
    assert_eq!(scans() - scanned, 2);
    assert!(watched_roots(&shared).is_empty());
    assert!(shared.lock().stream_failed.is_some());
    // Within the share gap windows are still not turned away.
    assert_eq!(shared.scan(&state, SHARING).get("project"), Some(&250));
    assert_eq!(scans() - scanned, 2);
}

#[test]
#[ignore = "slow: real FSEvents stream; waits for the system to report changes"]
fn a_stream_that_failed_is_tried_again_later() {
    let repo = Fixture::new();
    repo.file("a.rs", 100);
    let state = one_root(&repo);
    let shared = watching();
    shared.lock().stream_failed = Some(Instant::now());
    settle();
    assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&100));
    // Left alone for a while: still polling.
    assert!(watched_roots(&shared).is_empty());
    let scanned = scans();
    shared.scan(&state, ALWAYS);
    assert_eq!(scans() - scanned, 1);
    // Past the wait, the next scan makes the stream and it stops being polled.
    shared.lock().stream_failed = Some(ago(RETRY_STREAM));
    shared.scan(&state, ALWAYS);
    assert_eq!(watched_roots(&shared).len(), 1);
    let scanned = scans();
    shared.scan(&state, ALWAYS);
    assert!(
        shared
            .lock()
            .cache
            .roots
            .values()
            .all(|outcome| outcome.watched)
    );
    assert_eq!(scans() - scanned, 0);
}

#[test]
#[ignore = "slow: real FSEvents stream; waits for the system to report changes"]
fn a_stream_that_cannot_be_remade_leaves_the_one_that_works() {
    let one = Fixture::new();
    one.file("a.rs", 100);
    let two = Fixture::new();
    two.file("b.rs", 200);
    let state = state_of(vec![project("one", &[&one])]);
    let both = state_of(vec![project("one", &[&one]), project("two", &[&two])]);
    let shared = started(&state);
    let canonical = |fixture: &Fixture| fixture.0.canonicalize().unwrap();

    fsevents::REFUSE.with(|refuse| refuse.set(true));
    let scanned = scans();
    assert_eq!(shared.scan(&both, ALWAYS).get("two"), Some(&200));
    fsevents::REFUSE.with(|refuse| refuse.set(false));
    // The root that was watched still is, and the new one is polled.
    assert_eq!(watched_roots(&shared), [canonical(&one)]);
    assert!(shared.lock().stream_failed.is_some());
    {
        let inner = shared.lock();
        assert!(inner.cache.roots[&canonical(&one)].watched);
        assert!(!inner.cache.roots[&canonical(&two)].watched);
    }
    shared.scan(&both, ALWAYS);
    assert_eq!(
        scans() - scanned,
        2,
        "the new root every time, the old one never"
    );
    // Later the stream is made again and covers both.
    shared.lock().stream_failed = Some(ago(RETRY_STREAM));
    shared.scan(&both, ALWAYS);
    assert_eq!(watched_roots(&shared).len(), 2);
}

#[test]
#[ignore = "slow: real FSEvents stream; waits for the system to report changes"]
fn a_stream_that_is_replaced_still_delivers_the_changes_it_was_holding_back() {
    let (repo, a, _) = two_files();
    let new = Fixture::new();
    new.file("c.rs", 300);
    let state = one_root(&repo);
    let both = state_of(vec![project("project", &[&repo]), project("new", &[&new])]);
    settle();
    // A stream that holds changes back for as long as the app's does.
    let shared = Shared::watching(LATENCY);
    shared.scan(&state, ALWAYS);
    quiet(&shared, &state);
    // Whatever it was holding from the fixture is out by now.
    thread::sleep(LATENCY + Duration::from_millis(300));
    shared.scan(&state, ALWAYS);
    edit(&a, 250);
    // Long enough for the system to have the change, not for it to be handed on.
    thread::sleep(Duration::from_millis(300));
    let scanned = scans();
    // A project appears: the stream is made again, over one more root.
    assert_eq!(shared.scan(&both, ALWAYS).get("new"), Some(&300));
    assert_eq!(watched_roots(&shared).len(), 2);
    // The change is heard by the stream that was holding it back.
    assert!(wait_for(&shared, &both, "project", 250));
    assert_eq!(scans() - scanned, 1, "scanned more than the root that came");
}

#[test]
#[ignore = "slow: real FSEvents stream; waits for the system to report changes"]
fn roots_nobody_asks_about_any_more_stop_being_watched() {
    let one = Fixture::new();
    one.file("a.rs", 100);
    let two = Fixture::new();
    two.file("b.rs", 200);
    let both = state_of(vec![project("one", &[&one]), project("two", &[&two])]);
    let only_one = state_of(vec![project("one", &[&one])]);
    let shared = started(&both);
    let canonical = |fixture: &Fixture| fixture.0.canonicalize().unwrap();
    let mut both_roots = [canonical(&one), canonical(&two)];
    both_roots.sort();
    assert_eq!(watched_roots(&shared), both_roots);
    let sink = Arc::downgrade(&shared.lock().sink().unwrap());

    // Only one is asked about, for a while; nothing is forgotten yet.
    let age = |shared: &Shared, root: &Fixture, by: Duration| {
        let mut inner = shared.lock();
        *inner.asked.get_mut(&canonical(root)).unwrap() = ago(by);
        inner.swept = None;
    };
    age(&shared, &two, FORGET_AFTER - Duration::from_secs(60));
    shared.scan(&only_one, ALWAYS);
    assert_eq!(watched_roots(&shared).len(), 2);
    // Then it is long enough.
    age(&shared, &two, FORGET_AFTER + Duration::from_secs(1));
    assert_eq!(shared.scan(&only_one, ALWAYS).get("two"), None);
    assert_eq!(watched_roots(&shared), [canonical(&one)]);
    assert!(!shared.lock().cache.roots.contains_key(&canonical(&two)));
    // The stream over both is let go once it has handed over what it held back.
    thread::sleep(LATENCY_UNDER_TEST * 4);
    shared.scan(&only_one, ALWAYS);
    assert!(shared.lock().retiring.is_empty());
    assert!(let_go(&sink));

    // No root at all: no stream at all.
    age(&shared, &one, FORGET_AFTER + Duration::from_secs(1));
    shared.scan(&State::default(), ALWAYS);
    assert!(shared.lock().watch.is_none());
    assert!(watched_roots(&shared).is_empty());
}

#[test]
#[ignore = "slow: real FSEvents stream; waits for the system to report changes"]
fn dropping_the_cache_stops_the_stream() {
    let repo = Fixture::new();
    let file = repo.file("a.rs", 100);
    let state = one_root(&repo);
    let shared = started(&state);
    let sink = Arc::downgrade(&shared.lock().sink().unwrap());
    assert!(sink.upgrade().is_some());
    // Events are on their way when it goes.
    edit(&file, 200);
    drop(shared);
    assert!(let_go(&sink));
}

// Measurements, not tests.

/// CPU time this process has used, over all its threads.
fn process_cpu() -> Duration {
    // SAFETY: `usage` is valid for the call to fill in.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage
    };
    let time = |time: libc::timeval| Duration::new(time.tv_sec as u64, time.tv_usec as u32 * 1000);
    time(usage.ru_utime) + time(usage.ru_stime)
}

/// What the recency work of an idle app costs, with the registry of a real
/// installation, as three windows asking every 30 s each:
///
///   RIWORK_BENCH_STATE=~/.local/share/riwork/state.json RIWORK_BENCH_MINUTES=3 \
///     cargo test --release --bin riwork idle_cost -- --ignored --nocapture
///
/// First with a scan of every root each period, as before the stream, then with
/// the stream (`RIWORK_BENCH_ONLY=polling` or `=stream` runs just one). Reads the registry and watches and stats the files; changes
/// nothing. The machine is not idle, so the second figure includes whatever its
/// other work does to these directories.
#[test]
#[ignore]
fn idle_cost_of_a_real_registry() {
    use std::sync::atomic::Ordering::Relaxed;
    let path = std::env::var("RIWORK_BENCH_STATE").expect("RIWORK_BENCH_STATE");
    let minutes: u64 = std::env::var("RIWORK_BENCH_MINUTES")
        .ok()
        .and_then(|minutes| minutes.parse().ok())
        .unwrap_or(3);
    let mut state: State = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    // One more project, in a repository made for the purpose, that gets an edit
    // while the rest is idle: it shows the stream delivers with all the roots.
    let heartbeat = repository();
    let beating = heartbeat.file("pulse.rs", 100);
    heartbeat.git(&["add", "pulse.rs"]);
    state.projects.push(project("heartbeat", &[&heartbeat]));
    settle();
    let wanted = wanted_roots(&state);
    let roots: usize = wanted.iter().map(|(_, roots)| roots.len()).sum();
    println!("{roots} roots, {} projects", wanted.len());
    for (name, shared) in [
        ("polling (a scan of every root per period)", Shared::new()),
        ("FSEvents stream", Shared::watching(LATENCY)),
    ] {
        // RIWORK_BENCH_ONLY=stream (or polling) measures just one of them.
        if std::env::var("RIWORK_BENCH_ONLY").is_ok_and(|only| !name.contains(&only)) {
            continue;
        }
        let (cpu, wall, scanned) = (process_cpu(), Instant::now(), scans());
        let projects = shared.scan(&state, LIMITS).len();
        println!(
            "\n{name}: first scan {:?} wall, {:?} CPU, {} roots, {projects} projects",
            wall.elapsed(),
            process_cpu() - cpu,
            scans() - scanned
        );
        let sink = shared.lock().sink();
        if let Some(sink) = sink {
            let listed = |files: bool| {
                shared
                    .lock()
                    .cache
                    .listings
                    .values()
                    .filter(|listing| listing.files.is_some() == files)
                    .count()
            };
            println!(
                "  stream paths: {}, file lists: {} of repositories, {} of plain folders",
                sink.matcher().paths().len(),
                listed(true),
                listed(false)
            );
        }
        let dirty = shared
            .lock()
            .cache
            .roots
            .values()
            .filter(|outcome| outcome.dirty || !outcome.watched)
            .count();
        println!("  roots dirty or unwatched after the first scan: {dirty}");
        // Whatever the first scan itself provoked is heard before measuring.
        thread::sleep(Duration::from_secs(5));
        shared.scan(&state, LIMITS);
        let (cpu, wall, scanned) = (process_cpu(), Instant::now(), scans());
        let (all, kept) = (
            changes::heard::ALL.load(Relaxed),
            changes::heard::KEPT.load(Relaxed),
        );
        let mut asks = 0;
        let mut edited_at = None;
        let mut seen_after = None;
        edit(&beating, 100);
        while wall.elapsed() < Duration::from_secs(minutes * 60) {
            thread::sleep(ASK_EVERY / 3);
            let dates = shared.scan(&state, LIMITS);
            asks += 1;
            if edited_at.is_none() && wall.elapsed() > Duration::from_secs(60) {
                edit(&beating, 500);
                edited_at = Some(Instant::now());
            } else if let Some(at) = edited_at
                && seen_after.is_none()
                && dates.get("heartbeat") == Some(&500)
            {
                seen_after = Some(at.elapsed());
            }
        }
        let spent = process_cpu() - cpu;
        let minutes = wall.elapsed().as_secs_f64() / 60.0;
        println!(
            "  idle {minutes:.1} min, {asks} asks: {:?} CPU ({:.2}% of one core), {} root scans ({:.2}/min)",
            spent,
            100.0 * spent.as_secs_f64() / (minutes * 60.0),
            scans() - scanned,
            (scans() - scanned) as f64 / minutes,
        );
        println!(
            "  an edit to a file in a fresh repository was seen after {seen_after:?} (asked every {:?})",
            ASK_EVERY / 3
        );
        println!(
            "  events heard: {} ({:.0}/min), remembered: {}",
            changes::heard::ALL.load(Relaxed) - all,
            (changes::heard::ALL.load(Relaxed) - all) as f64 / minutes,
            changes::heard::KEPT.load(Relaxed) - kept,
        );
    }
}

/// What making and stopping a stream over a real registry costs.
///
///   RIWORK_BENCH_STATE=~/.local/share/riwork/state.json \
///     cargo test --release --bin riwork stream_cost -- --ignored --nocapture
#[test]
#[ignore]
fn stream_cost_of_a_real_registry() {
    let path = std::env::var("RIWORK_BENCH_STATE").expect("RIWORK_BENCH_STATE");
    let state: State = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let roots = roots_of(&state);
    // What every ask spends finding the roots, which a quiet stream leaves as
    // nearly all that is left.
    let before = process_cpu();
    for _ in 0..100 {
        std::hint::black_box(wanted_roots(&state));
    }
    println!(
        "wanted_roots: {:?} CPU per ask",
        (process_cpu() - before) / 100
    );
    let started = Instant::now();
    let sink = Arc::new(sink_for(&roots));
    let prepared = started.elapsed();
    let paths = sink.matcher().paths();
    let made = Instant::now();
    let stream = fsevents::Stream::start(&paths, LATENCY_UNDER_TEST, &sink).expect("a stream");
    let started_in = made.elapsed();
    let stopping = Instant::now();
    drop(stream);
    println!(
        "{} roots, {} stream paths: matcher {prepared:?}, stream made and started {started_in:?}, stopped {:?}",
        roots.len(),
        paths.len(),
        stopping.elapsed()
    );
}

/// What one event costs to hear, and to fold into the dates.
///
///   cargo test --release --bin riwork event_cost -- --ignored --nocapture
#[test]
#[ignore]
fn event_cost() {
    let (repo, a, _) = two_files();
    let root = repo.0.canonicalize().unwrap();
    let state = one_root(&repo);
    let sink = sink_for(std::slice::from_ref(&root));
    let path = |name: &str| root.join(name).as_os_str().as_bytes().to_vec();
    let counted = path("src/a.rs");
    let excluded = path("target/debug/deps/libsomething-0123456789abcdef.rlib");
    let outside = b"/Users/someone/elsewhere/entirely/not/watched/file.rs".to_vec();
    const COUNT: u32 = 1_000_000;
    for (name, bytes) in [
        ("in a root, counted", &counted),
        ("in a root, excluded directory", &excluded),
        ("in no root", &outside),
    ] {
        let events = vec![
            Event {
                path: bytes,
                flags: MODIFIED,
            };
            1000
        ];
        let before = process_cpu();
        for _ in 0..COUNT / 1000 {
            sink.record(events.iter().copied());
        }
        let spent = process_cpu() - before;
        println!(
            "hear one event {name}: {:.0} ns",
            spent.as_nanos() as f64 / f64::from(COUNT)
        );
        sink.take();
    }
    // Folding in a changed file: one lstat, one lookup of the file list.
    let mut heard = Heard::new(&state);
    let mut time = 1000;
    let before = process_cpu();
    const CHANGES: u32 = 20_000;
    for _ in 0..CHANGES {
        time += 1;
        set_time(&a, time);
        heard.hear(&a, MODIFIED | TOUCHED).apply();
    }
    let spent = process_cpu() - before;
    println!(
        "set the date of a file, hear it and fold it in: {:.1} us",
        spent.as_micros() as f64 / f64::from(CHANGES)
    );
    assert_eq!(heard.date("project"), Some(time));
}
