//! What a periodic worktree refresh may reuse. Discovery runs several Git
//! processes per repository, and every window repeats it for its project, so
//! the app remembers each answer with a stat-only fingerprint of the files Git
//! read to give it. An unchanged fingerprint means Git would answer the same,
//! and nothing is spawned. Explicit requests (CLI, MCP, creating a worktree,
//! adding a project) never come through here.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant, SystemTime},
};

use super::{
    DiscoveredWorktree, Discovery, Repository, discover_git_worktrees, has_git_marker,
    probe_repository,
};

/// A remembered answer is dropped after this long even if every fingerprinted
/// file looks unchanged. That bounds what a fingerprint cannot see, such as a
/// retargeted symlink or a Git setting outside the repositories.
pub(super) const MAX_AGE: Duration = Duration::from_secs(300);

/// A file changed this recently cannot be told apart from a later change in the
/// same clock tick, so a fingerprint that read one is never reused.
pub(super) const RACY_WINDOW: Duration = Duration::from_millis(30);

/// Windows refresh independently, so one project is otherwise scanned once per
/// window. Kept below the 10 s refresh period, so the window that synced last
/// is never itself skipped and a project is at most one period from its next
/// sync.
pub(super) const SYNC_MIN_GAP: Duration = Duration::from_secs(8);

const TABLE_LIMIT: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stamp {
    Missing,
    /// A directory whose entry list is not watched, only that it exists.
    Dir,
    Signature {
        modified: Option<SystemTime>,
        len: u64,
        inode: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// A directory's entry list: its modification time changes when a name is
    /// added, removed or renamed in it, but not when a file inside is edited.
    Entries,
    /// A path's existence and type, plus the signature of a file.
    Marker,
}

fn stamp(path: &Path, kind: Kind) -> Stamp {
    let metadata = match kind {
        Kind::Entries => fs::metadata(path),
        Kind::Marker => fs::symlink_metadata(path),
    };
    match metadata {
        Err(_) => Stamp::Missing,
        // A `.git` directory changes with every index lock; its existence is
        // what matters, and what is inside is fingerprinted file by file.
        Ok(metadata) if kind == Kind::Marker && metadata.is_dir() => Stamp::Dir,
        Ok(metadata) => Stamp::Signature {
            modified: metadata.modified().ok(),
            len: metadata.len(),
            inode: inode(&metadata),
        },
    }
}

#[cfg(unix)]
fn inode(metadata: &fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(metadata)
}

#[cfg(not(unix))]
fn inode(_: &fs::Metadata) -> u64 {
    0
}

/// The stamps of every path one result was computed from. Each is taken before
/// the path is read, so a change during the read shows up as a difference.
#[derive(Clone, Debug)]
pub(super) struct Fingerprint {
    stamps: BTreeMap<(PathBuf, Kind), Stamp>,
    racy: bool,
    since: Instant,
}

impl Default for Fingerprint {
    fn default() -> Self {
        Self {
            stamps: BTreeMap::new(),
            racy: false,
            since: Instant::now(),
        }
    }
}

impl Fingerprint {
    pub(super) fn entries(&mut self, directory: &Path) {
        self.record(directory, Kind::Entries);
    }

    pub(super) fn marker(&mut self, path: &Path) {
        self.record(path, Kind::Marker);
    }

    fn record(&mut self, path: &Path, kind: Kind) {
        let key = (path.to_owned(), kind);
        if self.stamps.contains_key(&key) {
            return;
        }
        let stamp = stamp(path, kind);
        if let Stamp::Signature {
            modified: Some(modified),
            ..
        } = stamp
        {
            // A modification time in the future is as untrustworthy as a fresh one.
            let settled = SystemTime::now()
                .duration_since(modified)
                .is_ok_and(|age| age >= RACY_WINDOW);
            self.racy |= !settled;
        }
        self.stamps.insert(key, stamp);
    }

    fn merge(&mut self, other: &Fingerprint) {
        for (key, stamp) in &other.stamps {
            self.stamps.entry(key.clone()).or_insert(*stamp);
        }
        self.racy |= other.racy;
        self.since = self.since.min(other.since);
    }

    /// True when a fresh stat of every path gives what was recorded.
    pub(super) fn is_current(&self) -> bool {
        !self.racy
            && self.since.elapsed() < MAX_AGE
            && self
                .stamps
                .iter()
                .all(|((path, kind), recorded)| stamp(path, *kind) == *recorded)
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.stamps.len()
    }

    /// Pretend the stamps were read `by` ago.
    #[cfg(test)]
    pub(super) fn backdate(&mut self, by: Duration) {
        self.since = self
            .since
            .checked_sub(by)
            .expect("uptime exceeds the maximum age");
    }
}

/// Fingerprint what Git reads to report a repository: where its Git directory
/// is, `HEAD` and the config of the main one, the list of linked worktrees and
/// each one's `HEAD` (the branch) and `gitdir` (the path). Returns false when
/// the Git directory cannot be found without running Git; nothing derived from
/// such a repository may be remembered.
pub(super) fn watch_repository(directory: &Path, fingerprint: &mut Fingerprint) -> bool {
    let dot_git = directory.join(".git");
    fingerprint.marker(&dot_git);
    let git_dir = match fs::symlink_metadata(&dot_git) {
        Ok(metadata) if metadata.is_dir() => dot_git,
        // A linked worktree or submodule: a file naming its Git directory.
        Ok(metadata) if metadata.is_file() => {
            let pointer = fs::read_to_string(&dot_git).ok().and_then(|text| {
                let target = text.trim().strip_prefix("gitdir:")?.trim().to_owned();
                (!target.is_empty()).then(|| directory.join(target))
            });
            match pointer {
                Some(pointer) => pointer,
                None => return false,
            }
        }
        Ok(_) => return false,
        Err(_) if has_git_marker(directory) => {
            // A bare repository is its own Git directory.
            fingerprint.marker(&directory.join("objects"));
            fingerprint.marker(&directory.join("refs"));
            directory.to_owned()
        }
        Err(_) => return false,
    };
    let pointer = git_dir.join("commondir");
    fingerprint.marker(&pointer);
    let common = match fs::read_to_string(&pointer) {
        Ok(text) => git_dir.join(text.trim()),
        Err(_) => git_dir.clone(),
    };
    fingerprint.marker(&git_dir.join("HEAD"));
    fingerprint.marker(&git_dir.join("config.worktree"));
    fingerprint.marker(&common.join("HEAD"));
    fingerprint.marker(&common.join("config"));
    // `git worktree add`, `remove` and `prune` create and delete entries here.
    let worktrees = common.join("worktrees");
    fingerprint.entries(&worktrees);
    if let Ok(entries) = fs::read_dir(&worktrees) {
        for entry in entries.flatten() {
            let path = entry.path();
            fingerprint.marker(&path.join("HEAD"));
            fingerprint.marker(&path.join("gitdir"));
        }
    }
    true
}

/// Git finds the repository of a directory by looking in it and in each parent
/// for `.git`, or for the files of a bare repository. A repository appearing
/// above a project changes what the project is, so all of them are watched.
fn watch_ancestors(directory: &Path, fingerprint: &mut Fingerprint) -> bool {
    let mut watched = true;
    for ancestor in directory.ancestors() {
        for name in [".git", "HEAD", "objects", "refs"] {
            fingerprint.marker(&ancestor.join(name));
        }
        if has_git_marker(ancestor) {
            watched &= watch_repository(ancestor, fingerprint);
        }
    }
    watched
}

struct Memo<T> {
    fingerprint: Fingerprint,
    value: T,
}

type Table<T> = Mutex<BTreeMap<PathBuf, Arc<Memo<T>>>>;

/// Whole-project results, by project root.
static PROJECTS: Table<Discovery> = Mutex::new(BTreeMap::new());
/// `probe_repository` for a directory found by the walk.
static PROBES: Table<Option<Repository>> = Mutex::new(BTreeMap::new());
/// `probe_repository` for a project root, which Git resolves through its parents.
static ANCESTOR_PROBES: Table<Option<Repository>> = Mutex::new(BTreeMap::new());
/// `discover_git_worktrees` for a repository root.
static LISTINGS: Table<Vec<DiscoveredWorktree>> = Mutex::new(BTreeMap::new());

fn lookup<T: Clone>(table: &Table<T>, key: &Path, into: Option<&mut Fingerprint>) -> Option<T> {
    // Stat outside the lock: a fingerprint can cover thousands of paths.
    let memo = table
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(key)
        .cloned()?;
    if !memo.fingerprint.is_current() {
        return None;
    }
    if let Some(into) = into {
        into.merge(&memo.fingerprint);
    }
    Some(memo.value.clone())
}

fn remember<T>(table: &Table<T>, key: &Path, fingerprint: Fingerprint, value: T) {
    if fingerprint.racy {
        return;
    }
    let mut table = table.lock().unwrap_or_else(PoisonError::into_inner);
    if table.len() >= TABLE_LIMIT {
        table.retain(|_, memo| memo.fingerprint.since.elapsed() < MAX_AGE);
        if table.len() >= TABLE_LIMIT {
            table.clear();
        }
    }
    table.insert(key.to_owned(), Arc::new(Memo { fingerprint, value }));
}

/// One discovery pass. A fresh pass runs Git for everything, as before. A
/// cached one answers from the memos when the fingerprints agree, and collects
/// the fingerprint of the whole pass so the next one can be skipped outright.
pub(super) struct Scan {
    cached: bool,
    pub(super) fingerprint: Fingerprint,
    /// False once the pass read something its fingerprint cannot cover.
    trackable: bool,
}

impl Scan {
    pub(super) fn fresh() -> Self {
        Self {
            cached: false,
            fingerprint: Fingerprint::default(),
            trackable: true,
        }
    }

    pub(super) fn cached() -> Self {
        Self {
            cached: true,
            ..Self::fresh()
        }
    }

    /// The project root as the store spells it: a retargeted symlink or a
    /// root that appears or vanishes changes everything below it.
    pub(super) fn root(&mut self, root: &Path) {
        if self.cached {
            self.fingerprint.marker(root);
        }
    }

    /// A directory the walk is about to list; a new repository can only appear
    /// as a new entry in one of them.
    pub(super) fn walk(&mut self, directory: &Path) {
        if self.cached {
            self.fingerprint.entries(directory);
        }
    }

    pub(super) fn probe_ancestor(&mut self, ancestor: &Path) -> Result<Option<Repository>, String> {
        if !self.cached {
            return probe_repository(ancestor);
        }
        if let Some(hit) = lookup(&ANCESTOR_PROBES, ancestor, Some(&mut self.fingerprint)) {
            return Ok(hit);
        }
        let mut watch = Fingerprint::default();
        let watched = watch_ancestors(ancestor, &mut watch);
        let result = probe_repository(ancestor);
        let keep = result.is_ok();
        self.finish(&ANCESTOR_PROBES, ancestor, watch, watched, &result, keep);
        result
    }

    pub(super) fn probe(&mut self, directory: &Path) -> Result<Option<Repository>, String> {
        if !self.cached {
            return probe_repository(directory);
        }
        if let Some(hit) = lookup(&PROBES, directory, Some(&mut self.fingerprint)) {
            return Ok(hit);
        }
        let mut watch = Fingerprint::default();
        let watched = watch_repository(directory, &mut watch);
        let result = probe_repository(directory);
        // Only a usable repository is remembered: the walk reports the others
        // as warnings, and a broken one may be repaired without a trace here.
        let keep = matches!(result, Ok(Some(_)));
        self.finish(&PROBES, directory, watch, watched, &result, keep);
        result
    }

    pub(super) fn worktrees(
        &mut self,
        repository: &Path,
    ) -> Result<Vec<DiscoveredWorktree>, String> {
        if !self.cached {
            return discover_git_worktrees(repository);
        }
        if let Some(hit) = lookup(&LISTINGS, repository, Some(&mut self.fingerprint)) {
            return Ok(hit);
        }
        let mut watch = Fingerprint::default();
        let watched = watch_repository(repository, &mut watch);
        let result = discover_git_worktrees(repository);
        if let Ok(found) = &result {
            // Whether a checkout still exists decides how its path is spelled.
            for worktree in found {
                watch.marker(&worktree.path);
            }
        }
        let keep = result.is_ok();
        self.finish(&LISTINGS, repository, watch, watched, &result, keep);
        result
    }

    fn finish<T: Clone>(
        &mut self,
        table: &Table<T>,
        key: &Path,
        watch: Fingerprint,
        watched: bool,
        result: &Result<T, String>,
        keep: bool,
    ) {
        self.fingerprint.merge(&watch);
        self.trackable &= watched;
        if let (true, true, Ok(value)) = (watched, keep, result) {
            remember(table, key, watch, value.clone());
        }
    }
}

/// The last discovery of this project, if nothing it read has changed.
pub(super) fn cached_discovery(root: &Path) -> Option<Discovery> {
    lookup(&PROJECTS, root, None)
}

pub(super) fn remember_discovery(root: &Path, scan: Scan, discovery: &Discovery) {
    if scan.cached && scan.trackable && discovery.complete {
        remember(&PROJECTS, root, scan.fingerprint, discovery.clone());
    }
}

/// Which windows may run a project's periodic sync right now. The GUI is one
/// process with a window per project view, and each window syncs for itself;
/// this lets one of them do it and the rest read the outcome from the store.
#[derive(Default)]
pub(super) struct SyncGate {
    table: Mutex<GateTable>,
}

#[derive(Default)]
struct GateTable {
    running: BTreeSet<String>,
    finished: BTreeMap<String, Instant>,
}

impl SyncGate {
    pub(super) const fn new() -> Self {
        Self {
            table: Mutex::new(GateTable {
                running: BTreeSet::new(),
                finished: BTreeMap::new(),
            }),
        }
    }

    /// Runs `work` unless `key` is running now or finished less than `min_gap`
    /// ago. Callers never wait for a run that is already under way.
    pub(super) fn run<T>(
        &self,
        key: &str,
        min_gap: Duration,
        work: impl FnOnce() -> T,
    ) -> Option<T> {
        {
            let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
            if table.running.contains(key)
                || table
                    .finished
                    .get(key)
                    .is_some_and(|finished| finished.elapsed() < min_gap)
            {
                return None;
            }
            table.running.insert(key.to_owned());
        }
        let _running = Running { gate: self, key };
        Some(work())
    }
}

struct Running<'a> {
    gate: &'a SyncGate,
    key: &'a str,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let mut table = self
            .gate
            .table
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        table.running.remove(self.key);
        table.finished.insert(self.key.to_owned(), Instant::now());
    }
}

pub(super) static SYNCS: SyncGate = SyncGate::new();
