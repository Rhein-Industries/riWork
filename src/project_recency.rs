//! Latest source-file edit times, shared by every window of the process.
//!
//! The scan runs on the caller's background thread, but only one runs at a time
//! and every window reads what it left in a process-wide cache. A window whose
//! roots are all cached and fresh gets the cached answer, one whose roots are
//! not waits for the scan in progress instead of starting its own, and a scan
//! that runs out of time leaves what the last one learned in place.
//!
//! Each root is scanned within its own budget. A root whose scan does not
//! complete contributes nothing, never a partial maximum, and a project shows the
//! latest edit among the roots that did complete, so one oversized or unreadable
//! root cannot erase the date of the others.
//!
//! Asking Git for the files of a repository is the expensive part, and the list
//! only changes when the index, the ignore rules or the entries of a directory
//! do. The list is remembered with a stat fingerprint of those, and an
//! unchanged one is reused: a scan then only stats the listed files, which is
//! what sees an edit to a tracked file, because that changes the file and
//! nothing the fingerprint holds.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc},
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};

use crate::store::{State, scan::Fingerprint};

const SCAN_TIMEOUT: Duration = Duration::from_secs(30);
const ROOT_TIMEOUT: Duration = Duration::from_secs(5);
const GIT_TIMEOUT: Duration = Duration::from_millis(1500);
/// Windows ask every 30 s each, so with several of them the process would scan
/// several times per period. A scan this recent answers all of them. Well below
/// the period, so a window is never turned away by its own last scan.
const SHARE_GAP: Duration = Duration::from_secs(15);
/// A root that no window has asked about for this long is forgotten.
const FORGET_AFTER: Duration = Duration::from_secs(30 * 60);
const MAX_ENTRIES: usize = 50_000;
const MAX_DEPTH: usize = 40;
const MAX_GIT_OUTPUT: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
struct Limits {
    scan_timeout: Duration,
    root_timeout: Duration,
    root_entries: usize,
    share_gap: Duration,
}

const LIMITS: Limits = Limits {
    scan_timeout: SCAN_TIMEOUT,
    root_timeout: ROOT_TIMEOUT,
    root_entries: MAX_ENTRIES,
    share_gap: SHARE_GAP,
};

#[cfg(test)]
thread_local! {
    /// Git commands started on this thread, so a test can prove a scan ran none.
    static GIT_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Return observed UNIX-second mtimes for projects with at least one completed
/// root scan. Missing roots and projects without regular source files have no
/// entry. Shared by all windows: see the module documentation.
pub fn scan(state: &State) -> BTreeMap<String, u64> {
    SHARED.scan(state, LIMITS)
}

static SHARED: Shared = Shared::new();

/// The registered roots of each project that exist, as directories.
type Wanted = Vec<(String, BTreeSet<PathBuf>)>;

fn wanted_roots(state: &State) -> Wanted {
    state
        .projects
        .iter()
        .map(|project| {
            let mut roots = BTreeSet::new();
            for path in std::iter::once(&project.root)
                .chain(project.repository_roots.iter())
                .chain(
                    state
                        .worktrees
                        .iter()
                        .filter(|worktree| worktree.project_id == project.id)
                        .map(|worktree| &worktree.path),
                )
            {
                // A root that cannot be resolved is skipped like a missing one.
                if let Ok(root) = path.canonicalize()
                    && fs::symlink_metadata(&root)
                        .is_ok_and(|metadata| metadata.file_type().is_dir())
                {
                    roots.insert(root);
                }
            }
            (project.id.clone(), roots)
        })
        .collect()
}

/// What the scans of this process remember. Entries are shared, so a copy is cheap.
#[derive(Clone)]
struct Cache {
    /// How the latest scan that finished ended, for each root.
    roots: BTreeMap<PathBuf, Arc<Outcome>>,
    /// The files Git listed for a directory, for as long as its fingerprint holds.
    listings: BTreeMap<PathBuf, Arc<Listing>>,
}

impl Cache {
    const fn new() -> Self {
        Self {
            roots: BTreeMap::new(),
            listings: BTreeMap::new(),
        }
    }
}

struct Outcome {
    result: Result<Option<u64>, ()>,
    at: Instant,
}

struct Listing {
    fingerprint: Fingerprint,
    /// None when Git says the directory is not in a repository.
    files: Option<Files>,
}

struct Files {
    /// The listed paths, NUL-terminated, minus those a scan never looks at.
    paths: Vec<u8>,
    /// Everything Git listed. Each entry counts against the root's budget.
    entries: usize,
}

struct Shared {
    inner: Mutex<Inner>,
    done: Condvar,
}

struct Inner {
    running: bool,
    finished: Option<Instant>,
    cache: Cache,
}

enum Turn {
    /// The cache holds what this caller needs.
    Read,
    /// A scan is under way and the cache lacks something this caller needs.
    Wait,
    Run,
}

impl Shared {
    const fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                running: false,
                finished: None,
                cache: Cache::new(),
            }),
            done: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn scan(&self, state: &State, limits: Limits) -> BTreeMap<String, u64> {
        let wanted = wanted_roots(state);
        let give_up = Instant::now() + limits.scan_timeout + limits.root_timeout;
        let mut inner = self.lock();
        loop {
            match Self::turn(&inner, &wanted, limits) {
                Turn::Read => break,
                Turn::Wait => {
                    let Some(patience) = give_up.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    inner = self
                        .done
                        .wait_timeout(inner, patience)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                Turn::Run => {
                    inner.running = true;
                    let cache = inner.cache.clone();
                    drop(inner);
                    let mut publish = Publish {
                        shared: self,
                        cache: None,
                    };
                    publish.cache = Some(scan_roots(cache, &wanted, limits));
                    drop(publish);
                    inner = self.lock();
                    break;
                }
            }
        }
        latest_edits(&inner.cache, &wanted)
    }

    fn turn(inner: &Inner, wanted: &Wanted, limits: Limits) -> Turn {
        let covered = wanted
            .iter()
            .flat_map(|(_, roots)| roots)
            .all(|root| inner.cache.roots.contains_key(root));
        if inner.running {
            return if covered { Turn::Read } else { Turn::Wait };
        }
        let fresh = inner
            .finished
            .is_some_and(|finished| finished.elapsed() < limits.share_gap);
        if covered && fresh {
            Turn::Read
        } else {
            Turn::Run
        }
    }
}

/// Puts a scan's outcome in the cache and wakes the callers waiting for it,
/// also when the scan panics, so a failed scan cannot hold everyone up.
struct Publish<'a> {
    shared: &'a Shared,
    cache: Option<Cache>,
}

impl Drop for Publish<'_> {
    fn drop(&mut self) {
        let mut inner = self.shared.lock();
        if let Some(cache) = self.cache.take() {
            inner.cache = cache;
            inner.finished = Some(Instant::now());
        }
        inner.running = false;
        self.shared.done.notify_all();
    }
}

/// Each project's newest edit among its roots that have a completed scan.
fn latest_edits(cache: &Cache, wanted: &Wanted) -> BTreeMap<String, u64> {
    wanted
        .iter()
        .filter_map(|(id, roots)| {
            let latest = roots.iter().filter_map(|root| cache.roots.get(root)).fold(
                None,
                |latest, outcome| match outcome.result {
                    Ok(time) => later(latest, time),
                    Err(()) => latest,
                },
            );
            latest.map(|latest| (id.clone(), latest))
        })
        .collect()
}

/// The roots `wanted` names, each once. Those never scanned go first, then the
/// least recently scanned, so a scan that keeps running out of time still gets
/// to all of them in turn.
fn due_first<'a>(wanted: &'a Wanted, cache: &Cache) -> Vec<&'a PathBuf> {
    let mut roots: Vec<&PathBuf> = Vec::new();
    for root in wanted.iter().flat_map(|(_, roots)| roots) {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots.sort_by_key(|root| cache.roots.get(*root).map(|outcome| outcome.at));
    roots
}

/// Scan the roots that `wanted` names within the overall deadline. A root the
/// deadline cuts short keeps what the last scan found, and one it never reaches
/// too, so a project does not lose its date for lack of time.
fn scan_roots(mut cache: Cache, wanted: &Wanted, limits: Limits) -> Cache {
    let deadline = Instant::now() + limits.scan_timeout;
    let roots = due_first(wanted, &cache);
    for root in &roots {
        if Instant::now() >= deadline {
            break;
        }
        let mut budget = Budget {
            deadline: deadline.min(Instant::now() + limits.root_timeout),
            remaining_entries: limits.root_entries,
        };
        let result = scan_root(root, &mut budget, &mut cache);
        if result.is_err() && Instant::now() >= deadline {
            break;
        }
        let at = Instant::now();
        cache
            .roots
            .insert((*root).clone(), Arc::new(Outcome { result, at }));
    }
    let asked: BTreeSet<&PathBuf> = roots.into_iter().collect();
    cache
        .roots
        .retain(|root, outcome| asked.contains(root) || outcome.at.elapsed() < FORGET_AFTER);
    let known: Vec<PathBuf> = cache
        .roots
        .keys()
        .cloned()
        .chain(asked.into_iter().cloned())
        .collect();
    cache
        .listings
        .retain(|directory, _| known.iter().any(|root| directory.starts_with(root)));
    cache
}

struct Budget {
    deadline: Instant,
    remaining_entries: usize,
}

impl Budget {
    fn check(&self) -> Result<(), ()> {
        if Instant::now() < self.deadline {
            Ok(())
        } else {
            Err(())
        }
    }

    fn consume(&mut self) -> Result<(), ()> {
        self.consume_many(1)
    }

    fn consume_many(&mut self, count: usize) -> Result<(), ()> {
        self.check()?;
        self.remaining_entries = self.remaining_entries.checked_sub(count).ok_or(())?;
        Ok(())
    }
}

fn later(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    a.into_iter().chain(b).max()
}

fn excluded_directory(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            ".git"
                | "target"
                | "node_modules"
                | "vendor"
                | "Pods"
                | "DerivedData"
                | ".venv"
                | "venv"
                | ".cache"
                | "__pycache__"
                | ".pytest_cache"
                | ".mypy_cache"
                | ".ruff_cache"
                | ".tox"
                | ".gradle"
                | ".next"
                | ".nuxt"
                | ".svelte-kit"
                | ".parcel-cache"
                | ".turbo"
                | ".Trash"
                | "dist"
                | "build"
                | "coverage"
        )
    )
}

fn ignored_file(name: &OsStr) -> bool {
    matches!(name.to_str(), Some(".DS_Store" | "Thumbs.db"))
}

fn regular_file_time(metadata: &fs::Metadata) -> Result<Option<u64>, ()> {
    if !metadata.file_type().is_file() {
        return Ok(None);
    }
    // A file dated before 1970 is real, just very old; it must not void the scan.
    Ok(Some(
        metadata
            .modified()
            .map_err(|_| ())?
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_secs()),
    ))
}

/// Entries that vanished or that the user cannot read hold no observable edit.
fn unobservable(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
    )
}

fn has_git_ancestor(root: &Path) -> bool {
    root.ancestors()
        .any(|path| fs::symlink_metadata(path.join(".git")).is_ok())
}

fn git_files(root: &Path, budget: &Budget) -> Result<Option<Vec<u8>>, ()> {
    budget.check()?;
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // A harness can set these variables for another repository. Never let that
    // redirect the project probe or inject Git configuration into this scan.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    #[cfg(test)]
    GIT_RUNS.with(|runs| runs.set(runs.get() + 1));
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) if !has_git_ancestor(root) => return Ok(None),
        Err(_) => return Err(()),
    };
    let stdout = child.stdout.take().ok_or(())?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout
            .take((MAX_GIT_OUTPUT + 1) as u64)
            .read_to_end(&mut output)
            .map(|_| output);
        let _ = sender.send(result);
    });
    let deadline = budget.deadline.min(Instant::now() + GIT_TIMEOUT);
    let mut output = None;
    loop {
        match receiver.try_recv() {
            Ok(Ok(bytes)) if bytes.len() <= MAX_GIT_OUTPUT => output = Some(bytes),
            Ok(_) | Err(mpsc::TryRecvError::Disconnected) if output.is_none() => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
            _ => {}
        }
        match child.try_wait() {
            Ok(Some(status)) if output.is_some() => {
                if status.success() {
                    return Ok(output);
                }
                return if has_git_ancestor(root) {
                    Err(())
                } else {
                    Ok(None)
                };
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
fn listed_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn listed_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).as_ref())
}

/// Whether a scan looks at an entry Git listed: not inside a directory it
/// leaves out and not a file that only the file manager makes.
fn considered(relative: &Path) -> bool {
    relative.components().all(|component| match component {
        Component::Normal(name) => !excluded_directory(name),
        _ => false,
    }) && !relative.file_name().is_some_and(ignored_file)
}

/// What Git reads to list `directory`: whether and where it is in a repository,
/// the index and `HEAD`, and the ignore rules that are not files of the listing.
/// The stamps are taken before Git runs, so a change during the run shows up as
/// a difference. False when the repository cannot be found without running Git,
/// and nothing listed for it may be remembered.
fn watch_repository(directory: &Path, fingerprint: &mut Fingerprint) -> bool {
    // Git looks in the directory and in each parent for `.git`, and the nearest
    // one wins, so a new one closer to the directory changes the answer.
    let mut top = None;
    for ancestor in directory.ancestors() {
        let dot_git = ancestor.join(".git");
        fingerprint.marker(&dot_git);
        if fs::symlink_metadata(&dot_git).is_ok() {
            top = Some(ancestor);
            break;
        }
    }
    let Some(top) = top else {
        // Not a repository; the stamps above are what would change that.
        return true;
    };
    let dot_git = top.join(".git");
    let git_dir = match fs::metadata(&dot_git) {
        Ok(metadata) if metadata.is_dir() => dot_git,
        // A linked worktree or submodule: a file naming its Git directory.
        Ok(metadata) if metadata.is_file() => {
            let target = fs::read_to_string(&dot_git).ok().and_then(|text| {
                let target = text.trim().strip_prefix("gitdir:")?.trim().to_owned();
                (!target.is_empty()).then_some(target)
            });
            match target {
                Some(target) => top.join(target),
                None => return false,
            }
        }
        _ => return false,
    };
    let pointer = git_dir.join("commondir");
    fingerprint.marker(&pointer);
    let common = match fs::read_to_string(&pointer) {
        Ok(text) => git_dir.join(text.trim()),
        Err(_) => git_dir.clone(),
    };
    // The index is the tracked half of the listing. Every command that adds,
    // removes or renames a tracked file, switches branch or commits rewrites it.
    fingerprint.marker(&git_dir.join("index"));
    fingerprint.marker(&git_dir.join("HEAD"));
    fingerprint.marker(&common.join("info").join("exclude"));
    fingerprint.marker(&common.join("config"));
    for ancestor in directory.ancestors() {
        fingerprint.marker(&ancestor.join(".gitignore"));
        if ancestor == top {
            break;
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".config")));
    if let Some(home) = home {
        fingerprint.marker(&home.join(".gitconfig"));
    }
    if let Some(config_home) = config_home {
        fingerprint.marker(&config_home.join("git").join("config"));
        fingerprint.marker(&config_home.join("git").join("ignore"));
    }
    true
}

impl Listing {
    /// The listing that `output` from `git ls-files` makes for `directory`. The
    /// untracked half of the listing changes when an entry is added to or
    /// removed from a directory that Git read, so those are stamped as well, and
    /// so is each `.gitignore` among the files.
    fn new(directory: &Path, output: Option<Vec<u8>>, mut fingerprint: Fingerprint) -> Self {
        let files = output.map(|output| {
            let mut paths = Vec::new();
            let mut entries = 0;
            let mut directories = BTreeSet::new();
            for bytes in output
                .split(|byte| *byte == 0)
                .filter(|bytes| !bytes.is_empty())
            {
                entries += 1;
                let relative = listed_path(bytes);
                if !considered(&relative) {
                    continue;
                }
                if relative.file_name() == Some(OsStr::new(".gitignore")) {
                    fingerprint.marker(&directory.join(&relative));
                }
                // Every directory on the way: an entry added to one that holds
                // only directories is as new a file as any other.
                let mut parent = relative.parent();
                while let Some(parent_directory) = parent
                    && !parent_directory.as_os_str().is_empty()
                    && directories.insert(parent_directory.to_owned())
                {
                    parent = parent_directory.parent();
                }
                paths.extend_from_slice(bytes);
                paths.push(0);
            }
            // Too long a list is never remembered, so nothing is stamped for it.
            if entries <= MAX_ENTRIES {
                fingerprint.entries(directory);
                for relative in directories {
                    fingerprint.entries(&directory.join(relative));
                }
            }
            Files { paths, entries }
        });
        Self { fingerprint, files }
    }
}

/// The files Git lists for `directory`. Git runs only when the last answer is
/// gone, past its age, or its fingerprint no longer matches.
fn repository_files(
    directory: &Path,
    budget: &Budget,
    cache: &mut Cache,
) -> Result<Arc<Listing>, ()> {
    if let Some(listing) = cache.listings.get(directory)
        && listing.fingerprint.is_current()
    {
        return Ok(listing.clone());
    }
    let mut fingerprint = Fingerprint::default();
    fingerprint.begin_read();
    let watched = watch_repository(directory, &mut fingerprint);
    let output = git_files(directory, budget)?;
    let listing = Arc::new(Listing::new(directory, output, fingerprint));
    // A list too long for any budget is not worth its memory: it fails at once.
    let small = listing
        .files
        .as_ref()
        .is_none_or(|files| files.entries <= MAX_ENTRIES);
    if watched && small && listing.fingerprint.settled() {
        cache.listings.insert(directory.to_owned(), listing.clone());
    } else {
        cache.listings.remove(directory);
    }
    Ok(listing)
}

/// The newest edit among the listed files, from the file system alone.
fn stat_files(root: &Path, files: &Files, budget: &mut Budget) -> Result<Option<u64>, ()> {
    budget.consume_many(files.entries)?;
    let mut latest = None;
    let mut checked_directories = BTreeSet::new();
    for bytes in files
        .paths
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
    {
        budget.check()?;
        let relative = listed_path(bytes);
        // Git does not traverse directory symlinks. Also check parents so a
        // changed path cannot make us inspect files outside the registered root.
        let mut parent = root.to_owned();
        let mut safe = true;
        let components: Vec<_> = relative.components().collect();
        for component in components.iter().take(components.len().saturating_sub(1)) {
            parent.push(component.as_os_str());
            if checked_directories.contains(&parent) {
                continue;
            }
            match fs::symlink_metadata(&parent) {
                Ok(metadata) if metadata.file_type().is_dir() => {
                    checked_directories.insert(parent.clone());
                }
                Ok(_) => {
                    safe = false;
                    break;
                }
                Err(error) if unobservable(&error) => {
                    safe = false;
                    break;
                }
                Err(_) => return Err(()),
            }
        }
        if !safe {
            continue;
        }
        match fs::symlink_metadata(root.join(relative)) {
            Ok(metadata) => latest = later(latest, regular_file_time(&metadata)?),
            Err(error) if unobservable(&error) => {}
            Err(_) => return Err(()),
        }
    }
    Ok(latest)
}

fn scan_root(root: &Path, budget: &mut Budget, cache: &mut Cache) -> Result<Option<u64>, ()> {
    let listing = repository_files(root, budget, cache)?;
    match &listing.files {
        Some(files) => stat_files(root, files, budget),
        None => scan_plain(root, budget, cache),
    }
}

fn scan_plain(root: &Path, budget: &mut Budget, cache: &mut Cache) -> Result<Option<u64>, ()> {
    let mut latest = None;
    let mut directories = vec![(root.to_owned(), 0)];
    while let Some((directory, depth)) = directories.pop() {
        budget.check()?;
        if directory != root && fs::symlink_metadata(directory.join(".git")).is_ok() {
            let listing = repository_files(&directory, budget, cache)?;
            let files = listing.files.as_ref().ok_or(())?;
            latest = later(latest, stat_files(&directory, files, budget)?);
            continue;
        }
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if unobservable(&error) => continue,
            Err(_) => return Err(()),
        };
        for entry in entries {
            budget.consume()?;
            let entry = entry.map_err(|_| ())?;
            let name = entry.file_name();
            if excluded_directory(&name) || ignored_file(&name) {
                continue;
            }
            let file_type = entry.file_type().map_err(|_| ())?;
            if file_type.is_dir() {
                if depth >= MAX_DEPTH {
                    return Err(());
                }
                directories.push((entry.path(), depth + 1));
            } else if file_type.is_file() {
                match fs::symlink_metadata(entry.path()) {
                    Ok(metadata) => latest = later(latest, regular_file_time(&metadata)?),
                    Err(error) if unobservable(&error) => {}
                    Err(_) => return Err(()),
                }
            }
        }
    }
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Project, Worktree};
    use std::{
        fs::{File, FileTimes},
        sync::Barrier,
    };

    /// One scan with nothing remembered from an earlier one.
    fn scan(state: &State) -> BTreeMap<String, u64> {
        scan_with(state, LIMITS)
    }

    fn scan_with(state: &State, limits: Limits) -> BTreeMap<String, u64> {
        Shared::new().scan(state, limits)
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("riwork-recency-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn file(&self, relative: &str, time: u64) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "source").unwrap();
            set_time(&path, time);
            path
        }

        fn state(&self) -> State {
            State {
                projects: vec![Project {
                    id: "project".to_owned(),
                    name: "Project".to_owned(),
                    root: self.0.clone(),
                    repository_roots: Vec::new(),
                    folder_id: None,
                    notify_on_agent_done: false,
                    codex_account: crate::store::ProjectCodexAccount::default(),
                    created_at: 999_999,
                }],
                ..State::default()
            }
        }

        fn git(&self, args: &[&str]) {
            let status = Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn set_time(path: &Path, time: u64) {
        File::open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(time)))
            .unwrap();
    }

    #[test]
    fn uses_file_edits_instead_of_directory_or_registration_times() {
        let fixture = Fixture::new();
        let source = fixture.file("src/main.rs", 200);
        fixture.file("target/generated", 900);
        fixture.file("node_modules/dependency.js", 800);
        fixture.file(".DS_Store", 700);
        let state = fixture.state();
        assert_eq!(scan(&state).get("project"), Some(&200));
        fs::write(&source, "changed source").unwrap();
        set_time(&source, 300);
        assert_eq!(scan(&state).get("project"), Some(&300));
    }

    #[test]
    fn git_uses_tracked_and_nonignored_untracked_files() {
        let fixture = Fixture::new();
        fixture.git(&["init", "--quiet"]);
        let ignore = fixture.file(".gitignore", 10);
        fs::write(&ignore, "generated/\n*.log\n").unwrap();
        set_time(&ignore, 10);
        fixture.file("tracked.rs", 20);
        fixture.git(&["add", ".gitignore", "tracked.rs"]);
        fixture.file("new.rs", 30);
        fixture.file("generated/output.rs", 900);
        fixture.file("debug.log", 800);
        // Even generated files already in the index are excluded.
        fixture.file("target/tracked-build", 700);
        fixture.git(&["add", "-f", "target/tracked-build"]);
        assert_eq!(scan(&fixture.state()).get("project"), Some(&30));
        fs::remove_file(fixture.0.join("tracked.rs")).unwrap();
        assert_eq!(scan(&fixture.state()).get("project"), Some(&30));
    }

    #[test]
    fn project_subdirectory_only_counts_files_inside_its_registered_root() {
        let fixture = Fixture::new();
        fixture.git(&["init", "--quiet"]);
        fixture.file("outside.rs", 900);
        fixture.file("package/source.rs", 20);
        fixture.git(&["add", "outside.rs", "package/source.rs"]);
        fixture.file("package/new.rs", 30);
        let mut state = fixture.state();
        state.projects[0].root = fixture.0.join("package");
        assert_eq!(scan(&state).get("project"), Some(&30));
    }

    #[test]
    fn includes_registered_repository_roots_and_git_worktrees() {
        let repository = Fixture::new();
        repository.git(&["init", "--quiet"]);
        repository.file("source.rs", 20);
        repository.git(&["add", "source.rs"]);
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
        let external = Fixture::new();
        let worktree = external.0.join("worktree");
        repository.git(&[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "recency-test",
            worktree.to_str().unwrap(),
        ]);
        set_time(&worktree.join("source.rs"), 100);
        let folder = Fixture::new();
        folder.file("notes.txt", 10);
        let mut state = folder.state();
        state.projects[0].repository_roots = vec![repository.0.clone()];
        state.worktrees.push(Worktree {
            id: "worktree".to_owned(),
            project_id: "project".to_owned(),
            branch: "recency-test".to_owned(),
            path: worktree,
            is_primary: false,
            repository_root: Some(repository.0.clone()),
            created_at: 999_999,
        });
        assert_eq!(scan(&state).get("project"), Some(&100));
    }

    #[test]
    fn plain_parent_respects_nested_repository_ignores() {
        let fixture = Fixture::new();
        fixture.file("notes.txt", 10);
        let nested = fixture.0.join("repository");
        fs::create_dir(&nested).unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&nested)
                .args(["init", "--quiet"])
                .status()
                .unwrap()
                .success()
        );
        let ignore = fixture.file("repository/.gitignore", 20);
        fs::write(&ignore, "generated/\n").unwrap();
        set_time(&ignore, 20);
        fixture.file("repository/source.rs", 30);
        fixture.file("repository/generated/output.rs", 900);
        assert_eq!(scan(&fixture.state()).get("project"), Some(&30));
    }

    #[test]
    fn missing_and_empty_roots_remain_unknown() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        assert!(scan(&state).is_empty());
        state.projects[0].root = fixture.0.join("missing");
        assert!(scan(&state).is_empty());
        state.projects[0].root = fixture.file("not-a-directory", 100);
        assert!(scan(&state).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn never_follows_internal_file_or_directory_symlinks() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        fixture.file("source.rs", 20);
        let external = Fixture::new();
        let outside = external.file("outside.rs", 900);
        symlink(&outside, fixture.0.join("linked.rs")).unwrap();
        symlink(&external.0, fixture.0.join("linked-directory")).unwrap();
        symlink(&fixture.0, fixture.0.join("loop")).unwrap();
        assert_eq!(scan(&fixture.state()).get("project"), Some(&20));
        fixture.git(&["init", "--quiet"]);
        fixture.git(&["add", "source.rs", "linked.rs", "linked-directory"]);
        assert_eq!(scan(&fixture.state()).get("project"), Some(&20));
    }

    fn with_root_entries(root_entries: usize) -> Limits {
        Limits {
            root_entries,
            ..LIMITS
        }
    }

    #[test]
    fn each_root_gets_its_own_entry_budget() {
        let first = Fixture::new();
        first.file("one.rs", 100);
        first.file("two.rs", 150);
        let second = Fixture::new();
        second.file("one.rs", 250);
        second.file("two.rs", 200);
        let mut state = first.state();
        state.projects[0].repository_roots = vec![second.0.clone()];
        // Each root needs exactly two entries; the project needs four.
        assert_eq!(
            scan_with(&state, with_root_entries(2)).get("project"),
            Some(&250)
        );
    }

    #[test]
    fn a_root_over_its_budget_does_not_erase_the_completed_roots() {
        let large = Fixture::new();
        for name in ["a.rs", "b.rs", "c.rs"] {
            large.file(name, 900);
        }
        let small = Fixture::new();
        small.file("only.rs", 40);
        let mut state = large.state();
        state.projects[0].repository_roots = vec![small.0.clone()];
        let limits = with_root_entries(2);
        assert_eq!(scan_with(&state, limits).get("project"), Some(&40));
        // With nothing complete, the project stays unknown rather than partial.
        state.projects[0].repository_roots.clear();
        assert!(scan_with(&state, limits).is_empty());
    }

    #[test]
    fn many_worktree_roots_do_not_share_one_budget() {
        let project = Fixture::new();
        project.file("source.rs", 10);
        let worktrees: Vec<_> = (0..6)
            .map(|index| {
                let worktree = Fixture::new();
                worktree.file("a.rs", 20 + index);
                worktree.file("b.rs", 30 + index);
                worktree
            })
            .collect();
        let mut state = project.state();
        state.worktrees = worktrees
            .iter()
            .enumerate()
            .map(|(index, worktree)| Worktree {
                id: format!("worktree-{index}"),
                project_id: "project".to_owned(),
                branch: format!("branch-{index}"),
                path: worktree.0.clone(),
                is_primary: false,
                repository_root: None,
                created_at: 999_999,
            })
            .collect();
        assert_eq!(
            scan_with(&state, with_root_entries(2)).get("project"),
            Some(&35)
        );
    }

    #[test]
    fn files_dated_before_1970_count_as_the_oldest_edit() {
        let fixture = Fixture::new();
        let old = fixture.file("old.rs", 0);
        File::open(&old)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH - Duration::from_secs(86_400)))
            .unwrap();
        assert_eq!(scan(&fixture.state()).get("project"), Some(&0));
        fixture.file("new.rs", 50);
        assert_eq!(scan(&fixture.state()).get("project"), Some(&50));
    }

    #[cfg(unix)]
    struct Unlock(PathBuf);

    #[cfg(unix)]
    impl Drop for Unlock {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }

    #[cfg(unix)]
    fn lock(directory: &Path) -> Unlock {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o000)).unwrap();
        Unlock(directory.to_owned())
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_subdirectory_does_not_erase_the_project_date() {
        let fixture = Fixture::new();
        fixture.file("src/main.rs", 40);
        fixture.file("private/secret.rs", 900);
        let _unlock = lock(&fixture.0.join("private"));
        assert_eq!(scan(&fixture.state()).get("project"), Some(&40));
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_directory_in_a_git_repository_is_skipped() {
        let fixture = Fixture::new();
        fixture.git(&["init", "--quiet"]);
        fixture.file("src/main.rs", 40);
        fixture.file("private/secret.rs", 900);
        fixture.git(&["add", "src/main.rs", "private/secret.rs"]);
        let _unlock = lock(&fixture.0.join("private"));
        assert_eq!(scan(&fixture.state()).get("project"), Some(&40));
    }

    #[test]
    fn rejects_partial_results_when_entry_budget_runs_out() {
        let fixture = Fixture::new();
        fixture.file("first.rs", 20);
        fixture.file("second.rs", 30);
        let mut budget = Budget {
            deadline: Instant::now() + Duration::from_secs(1),
            remaining_entries: 1,
        };
        assert_eq!(
            scan_plain(&fixture.0, &mut budget, &mut Cache::new()),
            Err(())
        );
    }

    /// A scan for every request: what the shared cache would otherwise answer.
    const ALWAYS: Limits = Limits {
        share_gap: Duration::ZERO,
        ..LIMITS
    };

    const SHARING: Limits = Limits {
        share_gap: Duration::from_secs(60),
        ..LIMITS
    };

    /// Files younger than the racy window are never trusted, so fixtures rest first.
    fn settle() {
        thread::sleep(Duration::from_millis(80));
    }

    fn git_runs() -> usize {
        GIT_RUNS.with(std::cell::Cell::get)
    }

    /// `work` and the number of Git commands this thread started while it ran.
    fn spawned<T>(work: impl FnOnce() -> T) -> (T, usize) {
        let before = git_runs();
        let result = work();
        (result, git_runs() - before)
    }

    fn git_in(directory: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn repository() -> Fixture {
        let fixture = Fixture::new();
        fixture.git(&["init", "--quiet"]);
        fixture
    }

    /// Rewrite a file the way an editor saves over it.
    fn edit(path: &Path, time: u64) {
        fs::write(path, "edited source").unwrap();
        set_time(path, time);
    }

    fn project(id: &str, roots: &[&Fixture]) -> Project {
        Project {
            id: id.to_owned(),
            name: id.to_owned(),
            root: roots[0].0.clone(),
            repository_roots: roots[1..].iter().map(|root| root.0.clone()).collect(),
            folder_id: None,
            notify_on_agent_done: false,
            codex_account: crate::store::ProjectCodexAccount::default(),
            created_at: 999_999,
        }
    }

    fn state_of(projects: Vec<Project>) -> State {
        State {
            projects,
            ..State::default()
        }
    }

    #[test]
    fn an_unchanged_root_runs_no_git() {
        let repo = repository();
        repo.file("src/a.rs", 100);
        repo.file("src/b.rs", 200);
        repo.git(&["add", "src"]);
        repo.file("new.rs", 300);
        let plain = Fixture::new();
        plain.file("notes.txt", 50);
        let folder = Fixture::new();
        folder.file("readme.txt", 10);
        let nested = folder.0.join("inner");
        fs::create_dir(&nested).unwrap();
        git_in(&nested, &["init", "--quiet"]);
        folder.file("inner/x.rs", 400);
        git_in(&nested, &["add", "x.rs"]);
        let state = state_of(vec![project("project", &[&repo, &plain, &folder])]);
        settle();
        let shared = Shared::new();

        // The repository, the plain directory and the folder each ask Git once
        // whether they are a repository, and so does the repository inside.
        let (first, runs) = spawned(|| shared.scan(&state, ALWAYS));
        assert_eq!(first.get("project"), Some(&400));
        assert_eq!(runs, 4);
        let (second, runs) = spawned(|| shared.scan(&state, ALWAYS));
        assert_eq!(second, first);
        assert_eq!(runs, 0);
    }

    #[test]
    fn an_edit_to_a_tracked_file_updates_the_date_without_git() {
        let repo = repository();
        let tracked = repo.file("src/tracked.rs", 100);
        repo.file("src/other.rs", 50);
        repo.git(&["add", "src"]);
        let state = state_of(vec![project("project", &[&repo])]);
        settle();
        let shared = Shared::new();
        assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&100));

        edit(&tracked, 250);
        let (edited, runs) = spawned(|| shared.scan(&state, ALWAYS));
        assert_eq!(edited.get("project"), Some(&250));
        assert_eq!(runs, 0);
        // A tracked file that is deleted no longer counts, as before.
        fs::remove_file(&tracked).unwrap();
        settle();
        assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&50));
    }

    #[test]
    fn adding_a_file_updates_the_file_list() {
        let repo = repository();
        repo.file("src/a.rs", 100);
        repo.file("pkg/inner/x.rs", 110);
        repo.git(&["add", "src", "pkg"]);
        let state = state_of(vec![project("project", &[&repo])]);
        settle();
        let shared = Shared::new();
        assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&110));

        // Into a directory that holds files, one that holds only a directory,
        // a new directory, and the root itself.
        for (path, time) in [
            ("src/b.rs", 200),
            ("pkg/direct.rs", 300),
            ("docs/deep/c.rs", 400),
            ("root.rs", 500),
        ] {
            repo.file(path, time);
            settle();
            let (found, runs) = spawned(|| shared.scan(&state, ALWAYS));
            assert_eq!(found.get("project"), Some(&time), "{path}");
            assert_eq!(runs, 1, "{path}");
            // The new list is remembered in turn.
            let (again, runs) = spawned(|| shared.scan(&state, ALWAYS));
            assert_eq!(again, found, "{path}");
            assert_eq!(runs, 0, "{path}");
        }
    }

    #[test]
    fn a_file_added_to_the_index_changes_the_file_list() {
        let repo = repository();
        let ignore = repo.file(".gitignore", 10);
        fs::write(&ignore, "*.log\n").unwrap();
        set_time(&ignore, 10);
        repo.file("a.rs", 100);
        repo.file("build.log", 900);
        repo.git(&["add", ".gitignore", "a.rs"]);
        let state = state_of(vec![project("project", &[&repo])]);
        settle();
        let shared = Shared::new();
        assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&100));

        // Forcing an ignored file into the index touches no directory.
        repo.git(&["add", "-f", "build.log"]);
        settle();
        let (found, runs) = spawned(|| shared.scan(&state, ALWAYS));
        assert_eq!(found.get("project"), Some(&900));
        assert_eq!(runs, 1);
    }

    #[test]
    fn a_changed_ignore_file_changes_the_file_list() {
        let repo = repository();
        let ignore = repo.file(".gitignore", 10);
        repo.file("a.rs", 100);
        repo.file("secret.rs", 900);
        repo.git(&["add", "a.rs"]);
        let state = state_of(vec![project("project", &[&repo])]);
        settle();
        let shared = Shared::new();
        assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&900));

        fs::write(&ignore, "secret.rs\n").unwrap();
        set_time(&ignore, 20);
        settle();
        let (found, runs) = spawned(|| shared.scan(&state, ALWAYS));
        assert_eq!(found.get("project"), Some(&100));
        assert_eq!(runs, 1);
    }

    #[test]
    fn a_changed_nested_ignore_file_changes_the_file_list() {
        let repo = repository();
        let ignore = repo.file("sub/.gitignore", 10);
        repo.file("sub/a.rs", 100);
        repo.file("sub/secret.rs", 900);
        repo.git(&["add", "sub/a.rs"]);
        let state = state_of(vec![project("project", &[&repo])]);
        settle();
        let shared = Shared::new();
        assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&900));

        fs::write(&ignore, "secret.rs\n").unwrap();
        set_time(&ignore, 20);
        settle();
        let (found, runs) = spawned(|| shared.scan(&state, ALWAYS));
        assert_eq!(found.get("project"), Some(&100));
        assert_eq!(runs, 1);
    }

    #[test]
    fn a_changed_exclude_file_changes_the_file_list() {
        let repo = repository();
        repo.file("a.rs", 100);
        repo.file("secret.rs", 900);
        repo.git(&["add", "a.rs"]);
        let state = state_of(vec![project("project", &[&repo])]);
        settle();
        let shared = Shared::new();
        assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&900));

        let exclude = repo.0.join(".git/info/exclude");
        fs::write(&exclude, "secret.rs\n").unwrap();
        settle();
        let (found, runs) = spawned(|| shared.scan(&state, ALWAYS));
        assert_eq!(found.get("project"), Some(&100));
        assert_eq!(runs, 1);
    }

    #[test]
    fn a_repository_appearing_in_a_plain_directory_is_noticed() {
        let fixture = Fixture::new();
        fixture.file("src/a.rs", 100);
        fixture.file("secret.rs", 200);
        let state = fixture.state();
        settle();
        let shared = Shared::new();
        assert_eq!(shared.scan(&state, ALWAYS).get("project"), Some(&200));
        assert_eq!(spawned(|| shared.scan(&state, ALWAYS)).1, 0);

        fixture.git(&["init", "--quiet"]);
        let ignore = fixture.file(".gitignore", 30);
        fs::write(&ignore, "secret.rs\n").unwrap();
        set_time(&ignore, 30);
        settle();
        let (found, runs) = spawned(|| shared.scan(&state, ALWAYS));
        assert_eq!(found.get("project"), Some(&100));
        assert_eq!(runs, 1);
    }

    #[test]
    fn concurrent_callers_share_one_scan() {
        let one = repository();
        one.file("a.rs", 100);
        one.git(&["add", "a.rs"]);
        let two = repository();
        two.file("b.rs", 200);
        let plain = Fixture::new();
        plain.file("c.txt", 300);
        let state = state_of(vec![
            project("first", &[&one, &plain]),
            project("second", &[&two, &plain]),
        ]);
        settle();
        let shared = Shared::new();
        let callers = 6;
        let barrier = Barrier::new(callers);
        let outcomes: Vec<_> = thread::scope(|scope| {
            let windows: Vec<_> = (0..callers)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        spawned(|| shared.scan(&state, SHARING))
                    })
                })
                .collect();
            windows
                .into_iter()
                .map(|window| window.join().unwrap())
                .collect()
        });
        // Every window got the whole answer, from one scan of the three roots.
        for (edits, _) in &outcomes {
            assert_eq!(edits.get("first"), Some(&300));
            assert_eq!(edits.get("second"), Some(&300));
        }
        let runs: usize = outcomes.iter().map(|(_, runs)| runs).sum();
        assert_eq!(runs, 3);
    }

    #[test]
    fn a_recent_scan_answers_later_callers_until_a_root_is_new() {
        let old = Fixture::new();
        let old_file = old.file("old.rs", 100);
        let young = Fixture::new();
        young.file("young.rs", 200);
        let mut state = state_of(vec![project("old", &[&old])]);
        settle();
        let shared = Shared::new();
        assert_eq!(shared.scan(&state, SHARING).get("old"), Some(&100));

        // Another window asks moments later: the answer is the cached one.
        edit(&old_file, 150);
        let (cached, runs) = spawned(|| shared.scan(&state, SHARING));
        assert_eq!(cached.get("old"), Some(&100));
        assert_eq!(runs, 0);
        // A project this scan never saw is not left waiting for the gap.
        state.projects.push(project("young", &[&young]));
        let (found, _) = spawned(|| shared.scan(&state, SHARING));
        assert_eq!(found.get("young"), Some(&200));
        assert_eq!(found.get("old"), Some(&150));
        // Once the gap has passed the next request scans again.
        let after_gap = Limits {
            share_gap: Duration::ZERO,
            ..SHARING
        };
        edit(&old_file, 175);
        assert_eq!(shared.scan(&state, after_gap).get("old"), Some(&175));
    }

    #[test]
    fn projects_the_deadline_cuts_off_keep_their_previous_date() {
        let first = Fixture::new();
        let first_file = first.file("a.rs", 100);
        let second = Fixture::new();
        let second_file = second.file("b.rs", 200);
        let state = state_of(vec![
            project("first", &[&first]),
            project("second", &[&second]),
        ]);
        let shared = Shared::new();
        let known = shared.scan(&state, ALWAYS);
        assert_eq!(known.get("first"), Some(&100));
        assert_eq!(known.get("second"), Some(&200));

        edit(&first_file, 500);
        edit(&second_file, 600);
        let out_of_time = Limits {
            scan_timeout: Duration::ZERO,
            ..ALWAYS
        };
        // Nothing was reached, and nothing was lost.
        assert_eq!(shared.scan(&state, out_of_time), known);
        // A scan with no earlier answer to keep has none to give.
        assert!(scan_with(&state, out_of_time).is_empty());
        let updated = shared.scan(&state, ALWAYS);
        assert_eq!(updated.get("first"), Some(&500));
        assert_eq!(updated.get("second"), Some(&600));
    }

    #[test]
    fn roots_never_scanned_go_before_the_ones_already_known() {
        let known = Fixture::new();
        known.file("a.rs", 100);
        let unknown = Fixture::new();
        unknown.file("b.rs", 200);
        let mut state = state_of(vec![project("known", &[&known])]);
        let shared = Shared::new();
        shared.scan(&state, ALWAYS);
        state
            .projects
            .insert(0, project("unknown", &[&unknown, &known]));
        let wanted = wanted_roots(&state);
        let cache = shared.lock().cache.clone();
        let order = due_first(&wanted, &cache);
        let canonical = |fixture: &Fixture| fixture.0.canonicalize().unwrap();
        assert_eq!(order, [&canonical(&unknown), &canonical(&known)]);
    }

    #[test]
    fn twenty_roots_ask_git_once_each_however_many_windows_ask_and_then_not_at_all() {
        let repositories: Vec<_> = (0..20)
            .map(|index| {
                let repo = repository();
                for file in 0..20 {
                    repo.file(&format!("src/f{file}.rs"), 100 + file);
                }
                repo.git(&["add", "src"]);
                repo.file("untracked.rs", 500 + index);
                repo
            })
            .collect();
        let state = state_of(
            (0..10)
                .map(|index| {
                    project(
                        &format!("project-{index}"),
                        &[&repositories[2 * index], &repositories[2 * index + 1]],
                    )
                })
                .collect(),
        );
        settle();
        let shared = Shared::new();

        // Seven windows ask within a period: the first scans, the rest read.
        let runs: Vec<_> = (0..7)
            .map(|_| spawned(|| shared.scan(&state, SHARING)).1)
            .collect();
        assert_eq!(runs, [20, 0, 0, 0, 0, 0, 0]);
        // The next period finds every file list as it was.
        let (edits, runs) = spawned(|| shared.scan(&state, ALWAYS));
        assert_eq!(runs, 0);
        assert_eq!(edits.len(), 10);
        for index in 0..10 {
            assert_eq!(
                edits.get(&format!("project-{index}")),
                Some(&(501 + 2 * index as u64))
            );
        }
    }

    #[test]
    fn the_public_scan_is_shared_by_every_window_of_the_process() {
        let fixture = Fixture::new();
        let file = fixture.file("a.rs", 100);
        let state = fixture.state();
        let (first, _) = spawned(|| super::scan(&state));
        assert_eq!(first.get("project"), Some(&100));
        edit(&file, 200);
        // Another window, a moment later: the cache, not another scan.
        let (second, runs) = spawned(|| super::scan(&state));
        assert_eq!(second, first);
        assert_eq!(runs, 0);
    }
}
