//! Remember presence learned from an explicit worktree refresh. Materializing
//! an all-project snapshot must never touch project directories or request access.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, PoisonError},
    time::{Duration, Instant},
};

const TTL: Duration = Duration::from_secs(2);
static PRESENCE: OnceLock<PresenceCache> = OnceLock::new();

struct PresenceCache {
    ttl: Duration,
    entries: Mutex<BTreeMap<PathBuf, (Instant, bool)>>,
}

impl PresenceCache {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    fn sample(&self, path: &Path, probe: impl FnOnce() -> bool) -> bool {
        // This function only runs in the blocking adapters. Holding the cache
        // lock through the stat guarantees one in-flight probe, including when
        // a watcher refresh races an explicit snapshot or another AppView.
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if let Some((observed, present)) = entries.get(path) {
            if now.saturating_duration_since(*observed) < self.ttl {
                return *present;
            }
        }
        // Retire stale paths when many projects have been visited, while
        // retaining every unexpired entry so its two-second promise holds.
        if entries.len() >= 4096 {
            entries.retain(|_, (observed, _)| now.saturating_duration_since(*observed) < self.ttl);
        }
        let present = probe();
        entries.insert(path.to_owned(), (Instant::now(), present));
        present
    }
}

pub(crate) fn known_present(path: &Path) -> bool {
    PRESENCE.get().and_then(|cache| {
        cache.entries.lock().unwrap_or_else(PoisonError::into_inner)
            .get(path).map(|(_, present)| *present)
    }).unwrap_or(true)
}

pub(crate) fn refresh<'a>(paths: impl Iterator<Item = &'a Path>) {
    let cache = PRESENCE.get_or_init(|| PresenceCache::new(TTL));
    for path in paths {
        cache.sample(path, || match std::fs::metadata(path) {
            Ok(metadata) => metadata.is_dir(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            // Denied access is not evidence a checkout was removed. Keep it
            // selectable; another explicit refresh may retry after permission.
            Err(_) => true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn concurrent_windows_and_snapshot_materialization_share_one_probe_per_path() {
        let cache = Arc::new(PresenceCache::new(Duration::from_secs(60)));
        let barrier = Arc::new(Barrier::new(12));
        let probes = Arc::new(AtomicUsize::new(0));
        let workers = (0..12)
            .map(|_| {
                let cache = cache.clone();
                let barrier = barrier.clone();
                let probes = probes.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    assert!(cache.sample(Path::new("/isolated/presence"), || {
                        probes.fetch_add(1, Ordering::SeqCst);
                        true
                    }));
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(probes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn missing_and_reappearing_directories_change_only_after_cache_expiry() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("worktree");
        let cache = PresenceCache::new(TTL);
        assert!(!cache.sample(&path, || path.is_dir()));
        std::fs::create_dir(&path).unwrap();
        assert!(!cache.sample(&path, || panic!("unexpired missing entry must be cached")));
        cache.entries.lock().unwrap().get_mut(&path).unwrap().0 = Instant::now() - TTL;
        assert!(cache.sample(&path, || path.is_dir()));
        std::fs::remove_dir(&path).unwrap();
        assert!(cache.sample(&path, || panic!("unexpired present entry must be cached")));
        cache.entries.lock().unwrap().get_mut(&path).unwrap().0 = Instant::now() - TTL;
        assert!(!cache.sample(&path, || path.is_dir()));
    }
}
