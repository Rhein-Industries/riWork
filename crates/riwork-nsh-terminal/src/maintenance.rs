//! Background maintenance coordination; no AppKit or terminal access here.
use std::{
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// A generation keeps registration/deadline notifications from being lost
/// between inspecting the registry and putting its worker to sleep.
#[derive(Default)]
pub(crate) struct Wake {
    generation: Mutex<u64>,
    changed: Condvar,
}

impl Wake {
    pub(crate) fn generation(&self) -> u64 {
        *self.generation.lock().unwrap()
    }

    pub(crate) fn notify(&self) {
        let mut generation = self.generation.lock().unwrap();
        *generation = generation.wrapping_add(1);
        self.changed.notify_one();
    }

    pub(crate) fn wait(&self, observed: u64, duration: Duration) -> u64 {
        let generation = self.generation.lock().unwrap();
        let (generation, _) = self
            .changed
            .wait_timeout_while(generation, duration, |current| *current == observed)
            .unwrap();
        *generation
    }
}

/// Live components still need the existing bounded prop/retry polling. With
/// none, only configuration and pooled surfaces have work on a deadline.
pub(crate) fn wait_duration(
    has_components: bool,
    pending: bool,
    config_after: Duration,
    pool_after: Option<Duration>,
) -> Duration {
    if has_components || pending {
        return crate::policy::MIN_RECHECK;
    }
    pool_after
        .map_or(config_after, |pool| pool.min(config_after))
        .max(crate::policy::MIN_RECHECK)
}

/// Retain live registrations without keeping their registry locked during work.
pub(crate) fn snapshot<T>(registry: &Mutex<Vec<Weak<T>>>) -> Vec<Arc<T>> {
    let mut registered = registry.lock().unwrap();
    registered.retain(|entry| entry.strong_count() > 0);
    registered.iter().filter_map(Weak::upgrade).collect()
}

/// One background producer and one main-queue consumer. Changes seen while a
/// pass is queued remain due after it finishes, and no backlog of passes forms.
pub(crate) struct Gate<T> {
    previous: Option<T>,
    queued: Arc<AtomicBool>,
    config_changed: bool,
}

impl<T> Default for Gate<T> {
    fn default() -> Self {
        Self {
            previous: None,
            queued: Arc::new(AtomicBool::new(false)),
            config_changed: false,
        }
    }
}

impl<T: PartialEq> Gate<T> {
    pub(crate) fn pending(&self) -> bool {
        self.config_changed || self.queued.load(Ordering::Acquire)
    }

    pub(crate) fn queue(&mut self, identity: T, due: bool, config_changed: bool) -> Option<Pass> {
        self.config_changed |= config_changed;
        if self.queued.load(Ordering::Acquire)
            || (!due && !self.config_changed && self.previous.as_ref() == Some(&identity))
        {
            return None;
        }
        self.previous = Some(identity);
        self.queued.store(true, Ordering::Release);
        Some(Pass {
            queued: self.queued.clone(),
            config_changed: std::mem::take(&mut self.config_changed),
        })
    }
}

pub(crate) struct Pass {
    queued: Arc<AtomicBool>,
    pub(crate) config_changed: bool,
}

impl Drop for Pass {
    fn drop(&mut self) {
        self.queued.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_before_and_during_wait_are_not_lost() {
        let wake = Arc::new(Wake::default());
        let observed = wake.generation();
        wake.notify();
        wake.notify();
        // Waiting with an older generation must return immediately even when
        // both notifications arrived before Condvar had a waiter.
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_wake = wake.clone();
        let worker = std::thread::spawn(move || {
            tx.send(worker_wake.wait(observed, Duration::from_secs(60)))
                .unwrap();
        });
        let current = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_ne!(current, observed);
        worker.join().unwrap();

        let (started, waiting) = std::sync::mpsc::channel();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_wake = wake.clone();
        let worker = std::thread::spawn(move || {
            started.send(()).unwrap();
            tx.send(worker_wake.wait(current, Duration::from_secs(60)))
                .unwrap();
        });
        waiting.recv().unwrap();
        wake.notify();
        assert_ne!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), current);
        worker.join().unwrap();
        // A timeout with no notification retains the observed generation.
        let current = wake.generation();
        assert_eq!(wake.wait(current, Duration::ZERO), current);
    }

    #[test]
    fn empty_registry_waits_for_real_deadlines_and_pending_work_keeps_polling() {
        use crate::policy::MIN_RECHECK;
        let config = Duration::from_secs(2);
        assert_eq!(wait_duration(false, false, config, None), config);
        assert_eq!(wait_duration(true, false, config, None), MIN_RECHECK);
        assert_eq!(wait_duration(false, true, config, None), MIN_RECHECK);
        assert_eq!(
            wait_duration(false, false, config, Some(Duration::from_secs(1))),
            Duration::from_secs(1)
        );
        assert_eq!(
            wait_duration(false, false, config, Some(Duration::ZERO)),
            MIN_RECHECK
        );
        assert_eq!(
            wait_duration(false, false, Duration::ZERO, None),
            MIN_RECHECK
        );

        let mut gate = Gate::default();
        let pass = gate.queue((), false, false).unwrap();
        assert!(gate.pending());
        assert!(gate.queue((), false, true).is_none());
        drop(pass);
        assert!(
            gate.pending(),
            "a coalesced config change still needs delivery"
        );
        drop(gate.queue((), false, false).unwrap());
        assert!(!gate.pending());
    }

    #[test]
    fn ten_empty_minutes_preserve_config_checks_with_one_eighth_the_wakeups() {
        fn simulate(live: bool) -> (usize, usize) {
            let mut now = Duration::ZERO;
            let mut check_at = now + Duration::from_secs(2);
            let mut checks = 0;
            let mut wakes = 0;
            while now < Duration::from_secs(600) {
                now += wait_duration(live, false, check_at.saturating_sub(now), None);
                wakes += 1;
                if now >= check_at {
                    checks += 1;
                    check_at = now + Duration::from_secs(2);
                }
            }
            (wakes, checks)
        }
        assert_eq!(simulate(true), (2400, 300));
        assert_eq!(simulate(false), (300, 300));
    }

    #[test]
    fn a_snapshot_releases_the_registry_before_work_and_keeps_its_entries_alive() {
        let first = Arc::new(7);
        let dead = Arc::new(9);
        let registry = Mutex::new(vec![Arc::downgrade(&first), Arc::downgrade(&dead)]);
        drop(dead);
        let held = snapshot(&registry);
        let mut unlocked = registry
            .try_lock()
            .expect("registration must not wait for snapshot users");
        assert_eq!(unlocked.len(), 1);
        unlocked.clear();
        drop(first);
        assert_eq!(**held.first().unwrap(), 7);
    }

    #[test]
    fn idle_ticks_only_dispatch_for_changes_or_deadlines_and_coalesce_pending_work() {
        let mut gate = Gate::default();
        let first = gate.queue(vec![1, 2], false, false).unwrap();
        // A blocked main queue sees at most one outstanding pass.
        for _ in 0..2400 {
            assert!(gate.queue(vec![1, 2], true, false).is_none());
        }
        assert!(gate.queue(vec![1, 3], false, true).is_none());
        drop(first);
        let second = gate.queue(vec![1, 3], false, false).unwrap();
        assert!(
            second.config_changed,
            "config changes survive queue coalescing"
        );
        drop(second);
        for _ in 0..2399 {
            assert!(gate.queue(vec![1, 3], false, false).is_none());
        }
        let deadline = gate.queue(vec![1, 3], true, false).unwrap();
        assert!(!deadline.config_changed);
        drop(deadline);
        assert!(gate.queue(vec![1], false, false).is_some());
    }
}
