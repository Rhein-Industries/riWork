//! One copy per process of what every window's periodic refresh reads from
//! tmux. Each window used to ask for the session list, the pane directories
//! and the process table itself, so N windows meant N sets of tmux and `ps`
//! processes every tick. Now the first window to ask in a tick does the work
//! and the rest read its answer. That work is one `tmux list-panes -a` (the
//! sessions that exist, the pane directories, the pane processes and what each
//! pane runs all come out of it) and, when a window shows them, one `ps`.
//!
//! Only the GUI's refresh asks for a sample. The CLI and MCP servers are other
//! processes that call `list` and friends directly, so they always see tmux as
//! it is; the sample is stale for the TTL at most, and never after this
//! process itself creates, closes or attaches a session.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use super::{PaneTable, SessionMetrics, ShellSession};

/// A little under the 2 s refresh period, so each tick of a window reads a
/// fresh sample while windows that tick together share one.
pub(super) const SAMPLE_TTL: Duration = Duration::from_millis(1500);

/// How long the server's default shell is trusted. It is a server option that
/// changes about never; the cost of asking is a tmux client per sample.
const SHELL_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug)]
pub struct SessionSample {
    pub shells: Vec<ShellSession>,
    /// `None` when not asked for, or when tmux or `ps` failed.
    pub metrics: Option<BTreeMap<String, SessionMetrics>>,
    /// The window 0 pane directory of each live shell. Always present in a
    /// sample that succeeded: it comes from the same tmux answer as the shells.
    pub directories: Option<BTreeMap<String, PathBuf>>,
}

/// Where a sample comes from: the registry, tmux and `ps` in the app, counters
/// in tests. `panes` is the one tmux query; the rest read its answer.
pub(super) trait SampleSource {
    /// The registry rows, read before tmux is asked.
    fn saved(&self) -> Result<Vec<ShellSession>, String>;
    fn panes(&self) -> Result<PaneTable, String>;
    fn shells(&self, saved: Vec<ShellSession>, panes: &PaneTable) -> Vec<ShellSession>;
    fn directories(&self, shells: &[ShellSession], panes: &PaneTable) -> BTreeMap<String, PathBuf>;
    fn metrics(
        &self,
        shells: &[ShellSession],
        panes: &PaneTable,
    ) -> Result<BTreeMap<String, SessionMetrics>, String>;
}

struct Entry {
    generation: u64,
    taken: Instant,
    sample: Result<SessionSample, String>,
    /// What tmux showed, kept for a window that asks for metrics later in the
    /// same tick.
    panes: Option<PaneTable>,
    metrics_taken: bool,
}

pub(super) struct SampleCache {
    ttl: Duration,
    /// Bumped by anything that changes sessions. A sample taken under an older
    /// generation is never served, even if it finishes after the change.
    generation: AtomicU64,
    /// Held while a sample is taken, so callers arriving meanwhile wait for it
    /// instead of taking their own.
    entry: Mutex<Option<Entry>>,
    /// The server's default shell, and when it was asked for.
    shell: Mutex<Option<(Instant, PathBuf)>>,
}

impl SampleCache {
    pub(super) const fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            generation: AtomicU64::new(0),
            entry: Mutex::new(None),
            shell: Mutex::new(None),
        }
    }

    pub(super) fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.shell.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The default shell of this tmux server. `ask` runs when nothing is
    /// remembered or the answer is older than `SHELL_TTL`; a change this process
    /// makes to sessions forgets it.
    pub(super) fn default_shell(&self, ask: impl FnOnce() -> PathBuf) -> PathBuf {
        let mut slot = self.shell.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((asked, shell)) = slot.as_ref()
            && asked.elapsed() < SHELL_TTL
        {
            return shell.clone();
        }
        let shell = ask();
        *slot = Some((Instant::now(), shell.clone()));
        shell
    }

    /// The current sample, taken now if the last one is older than the TTL or
    /// predates a change. A failure is served for the TTL too, so a wedged
    /// tmux costs one timeout per tick instead of one per window.
    pub(super) fn get(
        &self,
        source: &impl SampleSource,
        want_metrics: bool,
    ) -> Result<SessionSample, String> {
        let mut slot = self.entry.lock().unwrap_or_else(PoisonError::into_inner);
        let generation = self.generation.load(Ordering::SeqCst);
        if let Some(entry) = slot.as_mut()
            && entry.generation == generation
            && entry.taken.elapsed() < self.ttl
        {
            if want_metrics
                && !entry.metrics_taken
                && let (Ok(sample), Some(panes)) = (&mut entry.sample, &entry.panes)
            {
                sample.metrics = source.metrics(&sample.shells, panes).ok();
                entry.metrics_taken = true;
            }
            return entry.sample.clone();
        }
        let taken = Instant::now();
        let mut kept = None;
        let sample = source.saved().and_then(|saved| {
            let panes = source.panes()?;
            let shells = source.shells(saved, &panes);
            let sample = SessionSample {
                directories: Some(source.directories(&shells, &panes)),
                metrics: want_metrics
                    .then(|| source.metrics(&shells, &panes).ok())
                    .flatten(),
                shells,
            };
            kept = Some(panes);
            Ok(sample)
        });
        *slot = Some(Entry {
            generation,
            taken,
            sample: sample.clone(),
            panes: kept,
            metrics_taken: want_metrics,
        });
        sample
    }
}

/// The cache for one tmux server. Managers made from the same state directory
/// share it, whichever part of the app made them.
pub(super) fn cache_for(socket: &str) -> Arc<SampleCache> {
    static CACHES: Mutex<BTreeMap<String, Arc<SampleCache>>> = Mutex::new(BTreeMap::new());
    CACHES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(socket.to_owned())
        .or_insert_with(|| Arc::new(SampleCache::new(SAMPLE_TTL)))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[derive(Default)]
    struct Counting {
        /// tmux queries: the one thing a sample must not repeat.
        panes: AtomicU64,
        directories: AtomicU64,
        metrics: AtomicU64,
        failing: AtomicBool,
    }

    impl Counting {
        fn calls(&self) -> (u64, u64, u64) {
            (
                self.panes.load(Ordering::SeqCst),
                self.directories.load(Ordering::SeqCst),
                self.metrics.load(Ordering::SeqCst),
            )
        }
    }

    impl SampleSource for Counting {
        fn saved(&self) -> Result<Vec<ShellSession>, String> {
            Ok(vec![
                serde_json::from_value(serde_json::json!({
                    "id": "00000000-0000-4000-8000-000000000001",
                    "project_id": null,
                    "worktree_id": null,
                    "kind": "project",
                    "cwd": "/tmp",
                    "command": null,
                    "created_at_unix": 0
                }))
                .unwrap(),
            ])
        }

        fn panes(&self) -> Result<PaneTable, String> {
            self.panes.fetch_add(1, Ordering::SeqCst);
            if self.failing.load(Ordering::SeqCst) {
                return Err("tmux timed out".to_owned());
            }
            Ok(PaneTable::default())
        }

        fn shells(&self, saved: Vec<ShellSession>, _: &PaneTable) -> Vec<ShellSession> {
            saved
        }

        fn directories(&self, shells: &[ShellSession], _: &PaneTable) -> BTreeMap<String, PathBuf> {
            self.directories.fetch_add(1, Ordering::SeqCst);
            shells
                .iter()
                .map(|shell| (shell.id.clone(), PathBuf::from("/work")))
                .collect()
        }

        fn metrics(
            &self,
            shells: &[ShellSession],
            _: &PaneTable,
        ) -> Result<BTreeMap<String, SessionMetrics>, String> {
            self.metrics.fetch_add(1, Ordering::SeqCst);
            Ok(shells
                .iter()
                .map(|shell| (shell.id.clone(), SessionMetrics::default()))
                .collect())
        }
    }

    #[test]
    fn every_window_in_a_tick_reads_one_sample() {
        let cache = SampleCache::new(Duration::from_secs(60));
        let source = Counting::default();
        for _ in 0..7 {
            let sample = cache.get(&source, true).unwrap();
            assert_eq!(sample.shells.len(), 1);
            assert!(sample.metrics.is_some());
            assert_eq!(sample.directories.unwrap().len(), 1);
        }
        assert_eq!(source.calls(), (1, 1, 1));
    }

    #[test]
    fn concurrent_windows_share_the_sample_being_taken() {
        let cache = SampleCache::new(Duration::from_secs(60));
        let source = Counting::default();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| cache.get(&source, true).unwrap());
            }
        });
        assert_eq!(source.calls(), (1, 1, 1));
    }

    #[test]
    #[ignore = "slow: wall-clock TTL expiry"]
    fn a_sample_expires_after_its_ttl() {
        let cache = SampleCache::new(Duration::from_millis(40));
        let source = Counting::default();
        cache.get(&source, false).unwrap();
        cache.get(&source, false).unwrap();
        assert_eq!(source.calls(), (1, 1, 0));
        std::thread::sleep(Duration::from_millis(80));
        cache.get(&source, false).unwrap();
        assert_eq!(source.calls(), (2, 2, 0));
    }

    #[test]
    fn the_default_shell_is_asked_once_until_a_session_change() {
        let cache = SampleCache::new(Duration::from_secs(60));
        let asked = AtomicU64::new(0);
        let ask = || {
            asked.fetch_add(1, Ordering::SeqCst);
            PathBuf::from("/bin/zsh")
        };
        for _ in 0..5 {
            assert_eq!(cache.default_shell(ask), PathBuf::from("/bin/zsh"));
        }
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        cache.invalidate();
        cache.default_shell(ask);
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_change_made_by_this_process_discards_the_sample() {
        let cache = SampleCache::new(Duration::from_secs(60));
        let source = Counting::default();
        cache.get(&source, true).unwrap();
        cache.invalidate();
        cache.get(&source, true).unwrap();
        assert_eq!(source.calls(), (2, 2, 2));
        cache.get(&source, true).unwrap();
        assert_eq!(source.calls(), (2, 2, 2));
    }

    #[test]
    fn a_sample_that_finishes_after_a_change_is_not_served_again() {
        struct Slow<'a> {
            cache: &'a SampleCache,
            inner: Counting,
        }
        impl SampleSource for Slow<'_> {
            fn saved(&self) -> Result<Vec<ShellSession>, String> {
                self.inner.saved()
            }
            fn panes(&self) -> Result<PaneTable, String> {
                // The session change lands while tmux is being read.
                let panes = self.inner.panes();
                self.cache.invalidate();
                panes
            }
            fn shells(&self, saved: Vec<ShellSession>, panes: &PaneTable) -> Vec<ShellSession> {
                self.inner.shells(saved, panes)
            }
            fn directories(
                &self,
                shells: &[ShellSession],
                panes: &PaneTable,
            ) -> BTreeMap<String, PathBuf> {
                self.inner.directories(shells, panes)
            }
            fn metrics(
                &self,
                shells: &[ShellSession],
                panes: &PaneTable,
            ) -> Result<BTreeMap<String, SessionMetrics>, String> {
                self.inner.metrics(shells, panes)
            }
        }
        let cache = SampleCache::new(Duration::from_secs(60));
        let source = Slow {
            cache: &cache,
            inner: Counting::default(),
        };
        cache.get(&source, false).unwrap();
        cache.get(&source, false).unwrap();
        assert_eq!(source.inner.calls().0, 2);
    }

    #[test]
    fn metrics_are_taken_only_for_windows_that_show_them() {
        let cache = SampleCache::new(Duration::from_secs(60));
        let source = Counting::default();
        assert!(cache.get(&source, false).unwrap().metrics.is_none());
        assert_eq!(source.calls(), (1, 1, 0));
        // A window that shows them adds them to the same sample.
        assert!(cache.get(&source, true).unwrap().metrics.is_some());
        assert_eq!(source.calls(), (1, 1, 1));
        assert!(cache.get(&source, true).unwrap().metrics.is_some());
        assert!(cache.get(&source, false).unwrap().metrics.is_some());
        assert_eq!(source.calls(), (1, 1, 1));
    }

    #[test]
    fn a_failing_tmux_is_asked_once_per_ttl_not_once_per_window() {
        let cache = SampleCache::new(Duration::from_secs(60));
        let source = Counting::default();
        source.failing.store(true, Ordering::SeqCst);
        for _ in 0..5 {
            assert_eq!(cache.get(&source, true).unwrap_err(), "tmux timed out");
        }
        assert_eq!(source.calls(), (1, 0, 0));
        // Recovery is seen as soon as the failure is invalidated or expires.
        source.failing.store(false, Ordering::SeqCst);
        cache.invalidate();
        assert!(cache.get(&source, true).is_ok());
    }

    #[test]
    fn caches_are_per_tmux_server() {
        let first = cache_for("riwork-sample-test-a");
        assert!(Arc::ptr_eq(&first, &cache_for("riwork-sample-test-a")));
        assert!(!Arc::ptr_eq(&first, &cache_for("riwork-sample-test-b")));
    }
}
