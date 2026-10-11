//! The fingerprints and caches behind the periodic worktree refresh: what
//! invalidates them, and that a refresh with nothing to learn runs no Git.
use super::*;
use std::{
    sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

/// Files younger than this make a fingerprint racy, so fixtures rest first.
fn settle() {
    std::thread::sleep(scan::RACY_WINDOW * 2);
}

fn git_commands() -> usize {
    GIT_COMMANDS.with(std::cell::Cell::get)
}

/// `work` and the number of Git commands this thread built while it ran.
fn spawned<T>(work: impl FnOnce() -> T) -> (T, usize) {
    let before = git_commands();
    let result = work();
    (result, git_commands() - before)
}

fn project_at(root: &Path) -> Project {
    Project {
        id: Uuid::new_v4().to_string(),
        name: "scan".to_owned(),
        root: root.to_owned(),
        repository_roots: Vec::new(),
        folder_id: None,
        notify_on_agent_done: false,
        codex_account: ProjectCodexAccount::default(),
        created_at: 0,
    }
}

/// One cached pass over a project, keeping the fingerprint of everything it read.
fn cached_scan(project: &Project) -> (Scan, Discovery) {
    let mut scan = Scan::cached();
    scan.root(&project.root);
    let found = scan_project(project, &mut scan).unwrap();
    (scan, found)
}

fn summary(discovery: &Discovery) -> String {
    format!(
        "{:?}",
        (
            discovery.exists,
            discovery.complete,
            &discovery.repository_roots,
            &discovery.worktrees
        )
    )
}

fn branches(discovery: &Discovery) -> BTreeMap<PathBuf, String> {
    discovery
        .worktrees
        .iter()
        .map(|worktree| (worktree.path.clone(), worktree.branch.clone()))
        .collect()
}

fn add_worktree(repository: &Path, branch: &str, path: &Path) {
    git(
        repository,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new(branch),
            path.as_os_str(),
        ],
    );
}

fn remove_worktree(repository: &Path, path: &Path) {
    git(
        repository,
        [
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--force"),
            path.as_os_str(),
        ],
    );
}

/// A container project with `count` repositories and a store that knows it.
fn populated(fixture: &Fixture, count: usize) -> (Store, Project, Vec<PathBuf>) {
    let store = fixture.store();
    let container = fixture.directory("container");
    let repositories = (0..count)
        .map(|index| fixture.repository(format!("container/repo-{index:02}"), true))
        .collect();
    let project = store.create_project(&container, None, true).unwrap();
    (store, project, repositories)
}

#[test]
fn a_repository_reached_through_a_linked_worktree_is_watched_through_its_main_one() {
    let fixture = Fixture::new();
    let container = fixture.directory("container");
    let main = fixture.repository("main", true);
    let linked = fixture.path("container/linked");
    add_worktree(&main, "feature", &linked);
    let project = project_at(&container);
    settle();
    let (scan, found) = cached_scan(&project);
    assert_eq!(found.repository_roots, vec![main.clone()]);
    assert_eq!(branches(&found)[&main], "main");
    assert!(scan.fingerprint.is_current());

    // The project holds only the linked checkout, whose `.git` file leads to
    // the main repository's `HEAD`.
    git(&main, ["switch", "-c", "topic"]);
    assert!(!scan.fingerprint.is_current());
    settle();
    let (_, found) = cached_scan(&project);
    assert_eq!(branches(&found)[&main], "topic");
}

#[test]
fn a_fingerprint_changes_when_a_nested_repository_appears_or_goes() {
    let fixture = Fixture::new();
    let container = fixture.directory("container");
    fixture.repository("container/app", true);
    fixture.directory("container/deep/a/b");
    let plain = fixture.directory("container/plain");
    let project = project_at(&container);
    settle();
    let (scan, first) = cached_scan(&project);
    assert!(scan.fingerprint.is_current());
    assert_eq!(first.repository_roots.len(), 1);

    let mut expected = 1;
    for (place, create) in [
        ("a new sibling", "container/second"),
        (
            "a repository deep in an existing tree",
            "container/deep/a/b/inner",
        ),
    ] {
        settle();
        let (scan, _) = cached_scan(&project);
        assert!(scan.fingerprint.is_current(), "{place}");
        fixture.repository(create, true);
        assert!(!scan.fingerprint.is_current(), "{place}");
        expected += 1;
        settle();
        let (_, found) = cached_scan(&project);
        assert_eq!(found.repository_roots.len(), expected, "{place}");
    }

    // `git init` in a directory that already exists adds only a `.git` entry.
    settle();
    let (scan, _) = cached_scan(&project);
    git(&plain, ["init", "--initial-branch=main", "--template="]);
    assert!(!scan.fingerprint.is_current());
    settle();
    let (scan, found) = cached_scan(&project);
    expected += 1;
    assert_eq!(found.repository_roots.len(), expected);

    fs::remove_dir_all(fixture.path("container/second")).unwrap();
    assert!(!scan.fingerprint.is_current());
    settle();
    let (_, found) = cached_scan(&project);
    assert_eq!(found.repository_roots.len(), expected - 1);
}

#[test]
fn a_repository_appearing_above_the_project_is_noticed() {
    let fixture = Fixture::new();
    let outer = fixture.directory("outer");
    let inner = fixture.directory("outer/project");
    let project = project_at(&inner);
    settle();
    let (scan, first) = cached_scan(&project);
    assert!(first.repository_roots.is_empty());
    assert!(scan.fingerprint.is_current());
    git(&outer, ["init", "--initial-branch=main", "--template="]);
    assert!(!scan.fingerprint.is_current());
    settle();
    let (_, found) = cached_scan(&project);
    assert_eq!(found.repository_roots, vec![outer]);
}

#[test]
fn everyday_git_use_and_edits_do_not_disturb_a_fingerprint() {
    let fixture = Fixture::new();
    let container = fixture.directory("container");
    let repository = fixture.repository("container/app", true);
    fs::write(repository.join("notes.txt"), "one\n").unwrap();
    git(&repository, ["add", "notes.txt"]);
    git(&repository, ["commit", "--no-gpg-sign", "-m", "notes"]);
    let project = project_at(&container);
    settle();
    let (scan, _) = cached_scan(&project);
    assert!(scan.fingerprint.is_current());

    // Editing a file in place, `git status` refreshing the index, and a plain
    // commit do not create or remove any directory entry a scan reads.
    fs::write(repository.join("notes.txt"), "two\n").unwrap();
    git(&repository, ["status", "--short"]);
    assert!(scan.fingerprint.is_current());
}

#[test]
fn a_change_during_a_long_read_is_not_trusted_even_when_stamped_after_it() {
    let fixture = Fixture::new();
    let directory = fixture.directory("watched");
    settle();
    let mut fingerprint = scan::Fingerprint::default();
    fingerprint.begin_read();
    // The read is under way when the entry appears, and it ends well before the
    // stamp is taken: only the moment the read began tells the two apart.
    fs::write(directory.join("entry"), "x").unwrap();
    settle();
    fingerprint.entries(&directory);
    assert!(!fingerprint.settled());
    assert!(!fingerprint.is_current());

    settle();
    let mut fingerprint = scan::Fingerprint::default();
    fingerprint.begin_read();
    fingerprint.entries(&directory);
    assert!(fingerprint.settled());
    assert!(fingerprint.is_current());
}

#[test]
fn a_fingerprint_expires_after_the_safety_maximum() {
    let fixture = Fixture::new();
    let directory = fixture.directory("watched");
    settle();
    let mut fingerprint = scan::Fingerprint::default();
    fingerprint.entries(&directory);
    assert!(fingerprint.is_current());
    fingerprint.backdate(scan::MAX_AGE + Duration::from_secs(1));
    assert!(!fingerprint.is_current());
}

#[test]
fn cached_results_always_match_what_git_says_after_each_change() {
    let fixture = Fixture::new();
    let container = fixture.directory("container");
    let app = fixture.repository("container/app", true);
    let project = project_at(&container);
    let check = |step: &str| {
        let cached = discover_project(&project, true).unwrap();
        let fresh = discover_project(&project, false).unwrap();
        assert_eq!(summary(&cached), summary(&fresh), "{step}");
        // The same again must not go stale either.
        settle();
        let cached = discover_project(&project, true).unwrap();
        assert_eq!(summary(&cached), summary(&fresh), "{step}, repeated");
    };
    settle();
    check("start");

    let inside = fixture.path("container/wt-inside");
    add_worktree(&app, "inside", &inside);
    check("a worktree inside the project");
    let outside = fixture.path("wt-outside");
    add_worktree(&app, "outside", &outside);
    check("a worktree outside the project");
    git(&outside, ["branch", "-m", "outside", "elsewhere"]);
    check("a renamed branch");
    git(&app, ["switch", "-c", "topic"]);
    check("a checkout in the main worktree");
    let moved = fixture.path("wt-moved");
    git(
        &app,
        [
            OsStr::new("worktree"),
            OsStr::new("move"),
            outside.as_os_str(),
            moved.as_os_str(),
        ],
    );
    check("a moved worktree");
    remove_worktree(&app, &inside);
    check("a removed worktree");
    fs::remove_dir_all(&moved).unwrap();
    check("a worktree deleted by hand");
    git(&app, ["worktree", "prune"]);
    check("a pruned worktree");
    fixture.repository("container/nested/second", true);
    check("a nested repository");
    fs::remove_dir_all(fixture.path("container/nested")).unwrap();
    check("a removed nested repository");
}

#[test]
fn a_refresh_with_nothing_new_runs_no_git_and_writes_nothing() {
    let fixture = Fixture::new();
    let (store, project, _) = populated(&fixture, 4);
    settle();

    let started = std::time::Instant::now();
    let (_, cold) = spawned(|| store.sync_worktrees_with(&project.id, true).unwrap());
    let cold_time = started.elapsed();
    let state = store.dir.join("state.json");
    let saved = fs::read(&state).unwrap();
    let modified = fs::metadata(&state).unwrap().modified().unwrap();

    let started = std::time::Instant::now();
    let (_, warm) = spawned(|| store.sync_worktrees_with(&project.id, true).unwrap());
    let warm_time = started.elapsed();
    assert_eq!(warm, 0);
    let (_, warm_again) = spawned(|| store.refresh_worktrees(&project.id).unwrap());
    assert_eq!(warm_again, 0);
    assert_eq!(fs::read(&state).unwrap(), saved);
    assert_eq!(fs::metadata(&state).unwrap().modified().unwrap(), modified);

    // An explicit sync (CLI, MCP, creating a worktree) still asks Git for
    // everything, however warm the cache is, and refreshes the cache too.
    let started = std::time::Instant::now();
    let (_, forced) = spawned(|| store.sync_worktrees(&project.id).unwrap());
    let forced_time = started.elapsed();
    assert!(forced >= 4 * 4, "{forced}");
    eprintln!(
        "4 repositories, git commands (wall time): forced {forced} ({forced_time:?}), \
         cold cached {cold} ({cold_time:?}), warm {warm} ({warm_time:?})"
    );
}

#[test]
#[ignore = "slow: builds 20 Git repositories to count Git commands"]
fn a_change_in_one_repository_reruns_git_for_that_repository_only() {
    let fixture = Fixture::new();
    let (store, project, repositories) = populated(&fixture, 20);
    settle();
    store.sync_worktrees_with(&project.id, true).unwrap();
    let (_, warm) = spawned(|| store.sync_worktrees_with(&project.id, true).unwrap());
    assert_eq!(warm, 0);

    git(&repositories[7], ["switch", "-c", "topic"]);
    let (synced, changed) = spawned(|| store.sync_worktrees_with(&project.id, true).unwrap());
    assert!(
        synced
            .iter()
            .any(|worktree| worktree.path == repositories[7] && worktree.branch == "topic")
    );
    // Probing the repository (three commands) and listing its worktrees (one).
    assert!(changed <= 6, "{changed}");
    eprintln!("git commands after one branch change among 20 repositories: {changed}");

    settle();
    let (_, settled) = spawned(|| store.sync_worktrees_with(&project.id, true).unwrap());
    assert!(settled <= 6, "{settled}");
    let (_, warm) = spawned(|| store.sync_worktrees_with(&project.id, true).unwrap());
    assert_eq!(warm, 0);

    // A file created in a repository changes a directory the walk lists, so
    // the walk repeats, but every repository still answers from memory.
    fs::write(repositories[3].join("created.txt"), "x").unwrap();
    settle();
    let (_, walked) = spawned(|| store.sync_worktrees_with(&project.id, true).unwrap());
    assert_eq!(walked, 0);
}

#[test]
fn worktrees_created_outside_the_app_appear_on_the_next_scan() {
    let fixture = Fixture::new();
    let (store, project, repositories) = populated(&fixture, 3);
    settle();
    assert_eq!(
        store.sync_worktrees_with(&project.id, true).unwrap().len(),
        4
    );
    let linked = fixture.path("made-by-hand");
    add_worktree(&repositories[1], "by-hand", &linked);
    let synced = store.sync_worktrees_with(&project.id, true).unwrap();
    assert_eq!(synced.len(), 5);
    let record = project_worktree(&store, &project.id, &linked);
    assert_eq!(record.branch, "by-hand");
    assert_eq!(record.repository_root, Some(repositories[1].clone()));

    git(&linked, ["branch", "-m", "by-hand", "renamed"]);
    store.sync_worktrees_with(&project.id, true).unwrap();
    assert_eq!(
        project_worktree(&store, &project.id, &linked).branch,
        "renamed"
    );
}

#[test]
fn a_scan_with_warnings_is_never_reused() {
    let fixture = Fixture::new();
    let root = fixture.directory("invalid");
    fs::write(root.join(".git"), "this is not valid Git metadata\n").unwrap();
    let project = project_at(&root);
    settle();
    for _ in 0..2 {
        let (found, commands) = spawned(|| discover_project(&project, true).unwrap());
        assert!(!found.complete);
        assert!(commands > 0);
    }
}

#[test]
fn concurrent_windows_syncing_one_project_run_one_scan() {
    let fixture = Fixture::new();
    let (store, project, _) = populated(&fixture, 6);
    settle();
    let (_, single) = spawned(|| discover_project(&project, false).unwrap());
    assert!(single > 0);

    let windows = 8;
    let barrier = Barrier::new(windows);
    let total: usize = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..windows)
            .map(|_| {
                scope.spawn(|| {
                    let store = store.clone();
                    barrier.wait();
                    spawned(|| store.refresh_worktrees(&project.id).unwrap()).1
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .sum()
    });
    assert_eq!(total, single);
}

#[test]
fn the_gate_admits_one_run_per_key_at_a_time() {
    let gate = scan::SyncGate::new();
    let runs = AtomicUsize::new(0);
    let (started_sender, started) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let released = released;
            gate.run("project", Duration::ZERO, || {
                runs.fetch_add(1, Ordering::SeqCst);
                started_sender.send(()).unwrap();
                released.recv().unwrap();
            })
        });
        started.recv().unwrap();
        // While it runs, the same project is refused however often it is asked
        // (and without waiting), but another project is not held up.
        let refused = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    gate.run("project", Duration::ZERO, || {
                        runs.fetch_add(1, Ordering::SeqCst);
                    })
                    .is_none()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .all(|handle| handle.join().unwrap());
        assert!(refused);
        assert!(gate.run("other", Duration::ZERO, || ()).is_some());
        release.send(()).unwrap();
    });
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[test]
fn the_gate_spaces_out_syncs_of_one_project() {
    let gate = scan::SyncGate::new();
    assert!(
        gate.run("project", Duration::from_secs(60), || ())
            .is_some()
    );
    // Another window's turn a moment later has nothing to add.
    assert!(
        gate.run("project", Duration::from_secs(60), || ())
            .is_none()
    );
    assert!(gate.run("other", Duration::from_secs(60), || ()).is_some());
    let gap = Duration::from_millis(40);
    std::thread::sleep(gap * 2);
    assert!(gate.run("project", gap, || ()).is_some());
}

#[test]
fn a_panicking_sync_does_not_block_the_project_forever() {
    let gate = scan::SyncGate::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        gate.run("project", Duration::ZERO, || panic!("scan failed"))
    }));
    assert!(outcome.is_err());
    assert!(gate.run("project", Duration::ZERO, || ()).is_some());
}
