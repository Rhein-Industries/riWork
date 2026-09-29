//! One copy per process of what every window's periodic refresh reads from
//! tmux. Each window used to ask for the session list, the pane directories
//! and the process table itself, so N windows meant N sets of tmux and `ps`
//! processes every tick. Now the first window to ask in a tick does the work
//! and the rest read its answer.
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

use super::{SessionMetrics, ShellSession};

/// A little under the 2 s refresh period, so each tick of a window reads a
/// fresh sample while windows that tick together share one.
pub(super) const SAMPLE_TTL: Duration = Duration::from_millis(1500);

#[derive(Clone, Debug)]
pub struct SessionSample {
    pub shells: Vec<ShellSession>,
    /// `None` when not asked for, or when tmux or `ps` failed.
    pub metrics: Option<BTreeMap<String, SessionMetrics>>,
    /// `None` when tmux failed.
    pub directories: Option<BTreeMap<String, PathBuf>>,
}

/// Where a sample comes from: tmux and `ps` in the app, counters in tests.
pub(super) trait SampleSource {
    fn shells(&self) -> Result<Vec<ShellSession>, String>;
    fn directories(&self, shells: &[ShellSession]) -> Result<BTreeMap<String, PathBuf>, String>;
    fn metrics(&self, shells: &[ShellSession]) -> Result<BTreeMap<String, SessionMetrics>, String>;
}

struct Entry {
    generation: u64,
    taken: Instant,
    sample: Result<SessionSample, String>,
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
}

impl SampleCache {
    pub(super) const fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            generation: AtomicU64::new(0),
            entry: Mutex::new(None),
        }
    }

    pub(super) fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
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
                && let Ok(sample) = &mut entry.sample
            {
                sample.metrics = source.metrics(&sample.shells).ok();
                entry.metrics_taken = true;
            }
            return entry.sample.clone();
        }
        let taken = Instant::now();
        let sample = source.shells().map(|shells| SessionSample {
            directories: source.directories(&shells).ok(),
            metrics: want_metrics.then(|| source.metrics(&shells).ok()).flatten(),
            shells,
        });
        *slot = Some(Entry {
            generation,
            taken,
            sample: sample.clone(),
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
        shells: AtomicU64,
        directories: AtomicU64,
        metrics: AtomicU64,
        failing: AtomicBool,
    }

    impl Counting {
        fn calls(&self) -> (u64, u64, u64) {
            (
                self.shells.load(Ordering::SeqCst),
                self.directories.load(Ordering::SeqCst),
                self.metrics.load(Ordering::SeqCst),
            )
        }
    }

    impl SampleSource for Counting {
        fn shells(&self) -> Result<Vec<ShellSession>, String> {
            self.shells.fetch_add(1, Ordering::SeqCst);
            if self.failing.load(Ordering::SeqCst) {
                return Err("tmux timed out".to_owned());
            }
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

        fn directories(
            &self,
            shells: &[ShellSession],
        ) -> Result<BTreeMap<String, PathBuf>, String> {
            self.directories.fetch_add(1, Ordering::SeqCst);
            Ok(shells
                .iter()
                .map(|shell| (shell.id.clone(), PathBuf::from("/work")))
                .collect())
        }

        fn metrics(
            &self,
            shells: &[ShellSession],
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
    fn the_shipped_ttl_is_about_one_tick() {
        assert_eq!(SAMPLE_TTL, Duration::from_millis(1500));
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
            fn shells(&self) -> Result<Vec<ShellSession>, String> {
                // The session change lands while tmux is being read.
                let shells = self.inner.shells();
                self.cache.invalidate();
                shells
            }
            fn directories(
                &self,
                shells: &[ShellSession],
            ) -> Result<BTreeMap<String, PathBuf>, String> {
                self.inner.directories(shells)
            }
            fn metrics(
                &self,
                shells: &[ShellSession],
            ) -> Result<BTreeMap<String, SessionMetrics>, String> {
                self.inner.metrics(shells)
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
