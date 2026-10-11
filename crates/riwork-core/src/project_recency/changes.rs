//! What the change stream heard, kept until a caller asks, and what it means for
//! the dates the scans found.
//!
//! The stream's callback runs for every file changed below a watched root,
//! including the tens of thousands a build writes to `target/`, so it only
//! sorts: it finds the roots a path is in, drops what a scan never looks at,
//! and remembers the rest by path, once however many times the file changed.
//! The callers do the thinking, at most once per period and only for what
//! changed ([`apply`]).
//!
//! An edit to a file that counts raises the root's newest date to the file's
//! date, read from the file system when the change is applied. Anything the
//! newest date cannot absorb marks the root for a scan of its own: the file that
//! held the newest date is gone, replaced by a link or dated earlier; a file
//! appeared that the file list does not have, which Git may or may not list; an
//! ignore rule, the index or the HEAD changed; or the stream lost events. A root
//! never needs more than the one scan, however much happened.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::OsStr,
    fs,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use super::{
    Cache, Listing, Outcome, considered, directory_is_inside,
    fsevents::{Event, Events, Listener, flag},
    listed_directory, listed_path, regular_file_time, unobservable,
};

#[cfg(test)]
pub(super) mod heard {
    //! How much the stream said and how much of it mattered, for measurements.
    use std::sync::atomic::AtomicUsize;

    pub static ALL: AtomicUsize = AtomicUsize::new(0);
    pub static KEPT: AtomicUsize = AtomicUsize::new(0);
}

/// The paths remembered for one root before it is simply scanned again.
pub(super) const MAX_PENDING: usize = 2048;

/// Events that name a directory whose contents are no longer known: events
/// below it were merged into this one, or dropped because the kernel or this
/// process was too far behind, or the directory itself moved or was mounted.
const UNKNOWN_BELOW: u32 = flag::MUST_SCAN_SUBDIRS
    | flag::USER_DROPPED
    | flag::KERNEL_DROPPED
    | flag::ROOT_CHANGED
    | flag::MOUNT
    | flag::UNMOUNT;
/// What happened to an item that changed the names of its directory.
const NAME_CHANGE: u32 = flag::ITEM_CREATED | flag::ITEM_REMOVED | flag::ITEM_RENAMED;
/// What happened to an item that can change its date. Without one of these an
/// item only changed its owner or an attribute, which no scan looks at.
const CONTENT: u32 =
    NAME_CHANGE | flag::ITEM_INODE_META_MOD | flag::ITEM_MODIFIED | flag::ITEM_CLONED;

/// Files in a Git directory that decide what Git lists, as Git names them there.
/// Everything else in it, the objects and refs, changes with every commit and
/// fetch and decides nothing.
fn listing_file(inside_git_directory: &[u8]) -> bool {
    matches!(
        inside_git_directory,
        b"index" | b"HEAD" | b"config" | b"config.worktree" | b"commondir" | b"info/exclude"
    )
}

/// Whether `inner` is `outer` or below it, on a directory boundary.
fn within(inner: &[u8], outer: &[u8]) -> bool {
    inner.starts_with(outer)
        && (inner.len() == outer.len() || inner[outer.len()] == b'/' || outer == b"/")
}

fn path_of(bytes: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(bytes))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// Files below the root are its sources. An index into `Matcher::roots`.
    Root(usize),
    /// The Git directory of the root's repository, outside the root.
    Git(usize),
    /// The Git directory a linked worktree shares with the others, outside the
    /// root. What it holds of the root's file list is its exclude file and
    /// configuration: its index and `HEAD` are another worktree's.
    Shared(usize),
}

/// Which roots the directories a stream watches belong to.
pub(super) struct Matcher {
    roots: Vec<PathBuf>,
    /// The Git directories each root was given, in the order of `roots`.
    git_dirs: Vec<Vec<PathBuf>>,
    /// Every directory that matters, and who it matters to.
    dirs: HashMap<Box<[u8]>, Vec<Owner>>,
}

impl Matcher {
    /// Each root with the Git directories of its repository that lie outside it:
    /// its own first, then the one it shares, if that is another.
    pub(super) fn new(roots: impl IntoIterator<Item = (PathBuf, Vec<PathBuf>)>) -> Self {
        let mut matcher = Self {
            roots: Vec::new(),
            git_dirs: Vec::new(),
            dirs: HashMap::new(),
        };
        for (root, git_dirs) in roots {
            let index = matcher.roots.len();
            matcher
                .dirs
                .entry(root.as_os_str().as_bytes().into())
                .or_default()
                .push(Owner::Root(index));
            for (nth, dir) in git_dirs.iter().enumerate() {
                matcher
                    .dirs
                    .entry(dir.as_os_str().as_bytes().into())
                    .or_default()
                    .push(if nth == 0 {
                        Owner::Git(index)
                    } else {
                        Owner::Shared(index)
                    });
            }
            matcher.roots.push(root);
            matcher.git_dirs.push(git_dirs);
        }
        matcher
    }

    /// Whether files below `root` are reported.
    pub(super) fn watches(&self, root: &Path) -> bool {
        self.dirs
            .get(root.as_os_str().as_bytes())
            .is_some_and(|owners| owners.iter().any(|owner| matches!(owner, Owner::Root(_))))
    }

    #[cfg(test)]
    pub(super) fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Whether this is what `Matcher::new` would make of `roots`.
    pub(super) fn is_for<'a>(
        &self,
        roots: impl IntoIterator<Item = (&'a PathBuf, &'a Vec<PathBuf>)>,
    ) -> bool {
        let mut mine = self.roots.iter().zip(&self.git_dirs);
        roots.into_iter().all(|theirs| mine.next() == Some(theirs)) && mine.next().is_none()
    }

    /// The directories to hand to the stream: those above are enough for the
    /// ones below, which the stream reports anyway.
    pub(super) fn paths(&self) -> Vec<&Path> {
        let mut all: Vec<&Path> = self.dirs.keys().map(|dir| path_of(dir)).collect();
        all.sort();
        let mut kept: Vec<&Path> = Vec::with_capacity(all.len());
        for path in all {
            // Everything below a path sorts right after it.
            if !kept.last().is_some_and(|last| path.starts_with(last)) {
                kept.push(path);
            }
        }
        kept
    }
}

#[derive(Default)]
struct RootPending {
    /// Paths below the root (relative to it) that changed, and how.
    files: HashMap<Box<[u8]>, u32>,
    /// Git's own files changed. The root is scanned again, and its file list
    /// stays if the fingerprint of everything Git reads still holds.
    rescan: bool,
    /// The file list can no longer be trusted, or too much changed to follow.
    relist: bool,
}

impl RootPending {
    fn is_empty(&self) -> bool {
        self.files.is_empty() && !self.rescan && !self.relist
    }
}

#[derive(Default)]
pub(super) struct Pending {
    touched: bool,
    /// Events were lost for the whole stream.
    everything: bool,
    roots: Vec<RootPending>,
}

#[cfg(test)]
impl Pending {
    pub(super) fn rescan(&self, root: usize) -> bool {
        self.roots[root].rescan
    }

    pub(super) fn relist(&self, root: usize) -> bool {
        self.roots[root].relist
    }

    pub(super) fn files(&self, root: usize) -> usize {
        self.roots[root].files.len()
    }
}

/// Receives the stream's events for a set of roots.
pub(super) struct Sink {
    matcher: Matcher,
    pending: Mutex<Pending>,
}

impl Listener for Sink {
    fn heard(&self, events: Events<'_>) {
        self.record(events);
    }

    fn lost(&self) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        pending.touched = true;
        pending.everything = true;
    }
}

impl Sink {
    pub(super) fn new(matcher: Matcher) -> Self {
        let pending = Self::empty(&matcher);
        Self {
            matcher,
            pending: Mutex::new(pending),
        }
    }

    fn empty(matcher: &Matcher) -> Pending {
        Pending {
            touched: false,
            everything: false,
            roots: (0..matcher.roots.len())
                .map(|_| RootPending::default())
                .collect(),
        }
    }

    pub(super) fn matcher(&self) -> &Matcher {
        &self.matcher
    }

    /// What was heard since the last call, if anything.
    pub(super) fn take(&self) -> Option<Pending> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        pending
            .touched
            .then(|| std::mem::replace(&mut *pending, Self::empty(&self.matcher)))
    }

    pub(super) fn record<'a>(&self, events: impl IntoIterator<Item = Event<'a>>) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        for event in events {
            #[cfg(test)]
            heard::ALL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if event.flags & flag::EVENT_IDS_WRAPPED != 0 {
                // The ids start again from zero: no telling what was missed.
                pending.touched = true;
                pending.everything = true;
            } else if event.flags & flag::HISTORY_DONE != 0 {
                // The path of this one is meaningless.
            } else if event.flags & UNKNOWN_BELOW != 0 {
                self.unknown_below(&mut pending, event.path);
            } else {
                self.note(&mut pending, event);
            }
        }
    }

    /// Events were merged or lost below `path`: every root or Git directory that
    /// has anything to do with it is no longer known.
    fn unknown_below(&self, pending: &mut Pending, path: &[u8]) {
        pending.touched = true;
        let mut found = false;
        for (dir, owners) in &self.matcher.dirs {
            let inside = within(path, dir);
            if !inside && !within(dir, path) {
                continue;
            }
            found = true;
            // In a directory of a root that no scan looks into nothing counts,
            // however much was lost there; a build can lose a great deal.
            let uncounted = inside
                && path[dir.len()..].strip_prefix(b"/").is_some_and(|below| {
                    match inside_git(below) {
                        Some(git) => !git.looked_into(),
                        None => !considered(path_of(below)),
                    }
                });
            for owner in owners {
                match *owner {
                    Owner::Root(_) if uncounted => {}
                    Owner::Root(index) | Owner::Git(index) | Owner::Shared(index) => {
                        pending.roots[index].relist = true
                    }
                }
            }
        }
        if !found {
            pending.everything = true;
        }
    }

    fn note(&self, pending: &mut Pending, event: Event<'_>) {
        let path = event.path;
        // A root can be inside another, and its Git directory inside a third:
        // every directory the path is below speaks for the roots it belongs to.
        for slash in (1..path.len()).filter(|&at| path[at] == b'/') {
            let Some(owners) = self.matcher.dirs.get(&path[..slash]) else {
                continue;
            };
            let relative = &path[slash + 1..];
            for owner in owners {
                match *owner {
                    Owner::Root(index) => {
                        if Self::in_root(&mut pending.roots[index], relative, event.flags) {
                            pending.touched = true;
                            #[cfg(test)]
                            heard::KEPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    Owner::Git(index) => {
                        if listing_file(relative) {
                            pending.roots[index].rescan = true;
                            pending.touched = true;
                        }
                    }
                    Owner::Shared(index) => {
                        if matches!(relative, b"config" | b"info/exclude") {
                            pending.roots[index].rescan = true;
                            pending.touched = true;
                        }
                    }
                }
            }
        }
    }

    /// Remember a change below a root. True when there was something to remember.
    fn in_root(entry: &mut RootPending, relative: &[u8], flags: u32) -> bool {
        if entry.relist {
            return false;
        }
        if let Some(git) = inside_git(relative) {
            if !git.looked_into() {
                return false;
            }
            match git.after {
                // The `.git` entry itself: a repository came or went.
                None if flags & NAME_CHANGE != 0 => entry.relist = true,
                Some(inside) if listing_file(inside) => entry.rescan = true,
                _ => return false,
            }
            return true;
        }
        if relative.is_empty() || flags & CONTENT == 0 || !considered(path_of(relative)) {
            return false;
        }
        if let Some(seen) = entry.files.get_mut(relative) {
            *seen |= flags;
        } else if entry.files.len() >= MAX_PENDING {
            // Following this many changes costs more than looking again.
            entry.files.clear();
            entry.relist = true;
        } else {
            entry.files.insert(relative.into(), flags);
        }
        true
    }
}

/// A path with a `.git` among its components, cut at the first.
struct GitPath<'a> {
    /// What leads to it, without the slash after. Empty for a `.git` at the top.
    before: &'a [u8],
    /// What follows it: `None` for the `.git` entry itself.
    after: Option<&'a [u8]>,
}

impl GitPath<'_> {
    /// Whether a scan goes into the directory that holds the `.git`. A Git
    /// directory in `node_modules` changes nothing a scan sees.
    fn looked_into(&self) -> bool {
        self.before.is_empty() || considered(path_of(self.before))
    }
}

fn inside_git(relative: &[u8]) -> Option<GitPath<'_>> {
    let mut start = 0;
    loop {
        let end = relative[start..]
            .iter()
            .position(|&byte| byte == b'/')
            .map_or(relative.len(), |slash| start + slash);
        if &relative[start..end] == b".git" {
            return Some(GitPath {
                before: &relative[..start.saturating_sub(1)],
                after: (end < relative.len()).then(|| &relative[end + 1..]),
            });
        }
        if end == relative.len() {
            return None;
        }
        start = end + 1;
    }
}

type Listings = BTreeMap<PathBuf, Arc<Listing>>;

/// What decides whether a file below a root counts.
enum Governs {
    /// Every file that is not in an excluded directory.
    Plain,
    /// The file list of the repository the file is in.
    Listed {
        directory: PathBuf,
        listing: Arc<Listing>,
    },
    /// There is no file list to ask, so the root has to be scanned again.
    Unknown,
}

/// What a scan of `root` consults for `path`: its own file list if Git listed
/// the root, else the list of the first repository on the way down.
fn governing(listings: &Listings, root: &Path, path: &Path) -> Governs {
    match listings.get(root) {
        None => return Governs::Unknown,
        Some(listing) if listing.files.is_some() => {
            return Governs::Listed {
                directory: root.to_owned(),
                listing: listing.clone(),
            };
        }
        Some(_) => {}
    }
    let Ok(below) = path.strip_prefix(root) else {
        return Governs::Plain;
    };
    let components: Vec<_> = below.components().collect();
    let mut directory = root.to_owned();
    // The last component is the path itself, not a directory it is in.
    for component in &components[..components.len().saturating_sub(1)] {
        directory.push(component);
        if let Some(listing) = listings.get(&directory)
            && listing.files.is_some()
        {
            return Governs::Listed {
                directory,
                listing: listing.clone(),
            };
        }
    }
    Governs::Plain
}

/// Forget what Git said of `governs` and have the root scanned again.
fn stale(listings: &mut Listings, governs: &Governs, outcome: &mut Outcome) {
    if let Governs::Listed { directory, .. } = governs {
        listings.remove(directory);
    }
    outcome.changed();
}

/// Fold what the stream heard into what the scans found.
pub(super) fn apply(cache: &mut Cache, matcher: &Matcher, pending: Pending) {
    let Cache { roots, listings } = cache;
    if pending.everything {
        listings.clear();
        for outcome in roots.values_mut() {
            Arc::make_mut(outcome).changed();
        }
        return;
    }
    for (entry, root) in pending.roots.into_iter().zip(&matcher.roots) {
        if entry.is_empty() {
            continue;
        }
        let Some(slot) = roots.get_mut(root) else {
            // Never scanned, or forgotten: the scan to come sees everything.
            continue;
        };
        if entry.relist {
            listings.retain(|directory, _| !directory.starts_with(root));
            Arc::make_mut(slot).changed();
            continue;
        }
        if slot.dirty || slot.result.is_err() {
            // Another scan is due that reads every file anyway.
            continue;
        }
        let outcome = Arc::make_mut(slot);
        if entry.rescan {
            outcome.changed();
            continue;
        }
        let mut checked = HashSet::new();
        for (relative, flags) in entry.files {
            file_changed(listings, root, outcome, &relative, flags, &mut checked);
            if outcome.dirty {
                break;
            }
        }
    }
}

/// One changed path below `root`, read from the file system as it is now.
/// `checked` holds the directories found to be real directories.
fn file_changed(
    listings: &mut Listings,
    root: &Path,
    outcome: &mut Outcome,
    relative: &[u8],
    flags: u32,
    checked: &mut HashSet<PathBuf>,
) {
    let path = root.join(listed_path(relative));
    let metadata = fs::symlink_metadata(&path);
    let governs = governing(listings, root, &path);
    if matches!(governs, Governs::Unknown) {
        outcome.changed();
        return;
    }
    // Git spells a name with its accents composed, a file system may not, and
    // the two are different bytes. Such a name cannot be told from the list.
    let spelled_either_way = !relative.is_ascii();
    let is_directory = match &metadata {
        Ok(metadata) => metadata.is_dir(),
        Err(_) => flags & flag::ITEM_IS_DIR != 0,
    };
    if is_directory {
        match metadata {
            // A directory that came or moved in holds files nobody has listed.
            Ok(_) if flags & NAME_CHANGE != 0 => stale(listings, &governs, outcome),
            Ok(_) => {}
            // Everything below it is gone.
            Err(_) if spelled_either_way || outcome.holder_below(&path) => outcome.changed(),
            Err(_) => {}
        }
        return;
    }
    if path.file_name() == Some(OsStr::new(".gitignore")) {
        // Changes what Git lists, wherever it is and whatever happened to it.
        stale(listings, &governs, outcome);
        return;
    }
    let (base, listed) = match &governs {
        Governs::Listed { directory, listing } => {
            let below = path.strip_prefix(directory).unwrap_or(&path);
            let listed = listing
                .files
                .as_ref()
                .is_some_and(|files| files.contains(below.as_os_str().as_bytes()));
            (directory.as_path(), listed)
        }
        _ => (root, true),
    };
    match metadata {
        Ok(metadata) if listed => match regular_file_time(&metadata) {
            Ok(Some(time)) => {
                // A scan does not go through a link to a directory, and a
                // directory may have become one since this was written.
                let below = path.strip_prefix(base).unwrap_or(&path);
                let directory = listed_directory(below.as_os_str().as_bytes());
                match directory_is_inside(base, directory, checked) {
                    Ok(true) => outcome.edited(time, path),
                    Ok(false) if outcome.holder(&path) => outcome.changed(),
                    Ok(false) => {}
                    Err(()) => outcome.changed(),
                }
            }
            // A link, or something else a scan would not count.
            Ok(None) if outcome.holder(&path) => outcome.changed(),
            Ok(None) => {}
            Err(()) => outcome.changed(),
        },
        // Git may list a file that is new, and does not list one it ignores. An
        // old file Git does not list is ignored: it changed, but was not new.
        Ok(_) if flags & NAME_CHANGE != 0 => stale(listings, &governs, outcome),
        Ok(_) if spelled_either_way => outcome.changed(),
        Ok(_) => {}
        Err(error) if unobservable(&error) => {
            if outcome.holder(&path) || (spelled_either_way && !listed) {
                outcome.changed();
            }
        }
        Err(_) => outcome.changed(),
    }
}
