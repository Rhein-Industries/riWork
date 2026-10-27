//! Test-only fixtures. Never compiled into the default/release native module.
pub mod fixtures;

use crate::{
    backend::SessionsBackend,
    dto::{SessionsSnapshot, ShellSessionDto},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// A deterministic clock shared by fake services; advancing it never sleeps.
#[derive(Clone, Default)]
pub struct ManualClock(Arc<AtomicU64>);

impl ManualClock {
    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }

    pub fn advance(&self, elapsed: Duration) {
        let millis = elapsed.as_millis().min(u64::MAX as u128) as u64;
        let mut now = self.0.load(Ordering::SeqCst);
        while let Err(current) = self.0.compare_exchange_weak(
            now,
            now.saturating_add(millis),
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            now = current;
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct FakeSession {
    #[serde(flatten)]
    pub session: ShellSessionDto,
    #[serde(default)]
    pub appear_after_ms: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct FakeSessionsFile {
    pub sessions: Vec<FakeSession>,
    #[serde(default)]
    pub create_failures: BTreeMap<String, String>,
}

enum Clock {
    Manual(ManualClock),
    Real(Instant),
}

/// Implements the same read side as production without opening tmux or a shell.
pub struct FakeSessions {
    fixture: FakeSessionsFile,
    clock: Clock,
}

impl FakeSessions {
    pub fn new(fixture: FakeSessionsFile, clock: ManualClock) -> Self {
        Self {
            fixture,
            clock: Clock::Manual(clock),
        }
    }

    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            fixture: serde_json::from_slice(&std::fs::read(path)?)?,
            clock: Clock::Real(Instant::now()),
        })
    }

    /// Mutation tests can consume the fixture's deterministic creation failure.
    /// Successful session creation is deliberately not part of the P1 bridge.
    pub fn create_failure(&self, project_id: &str) -> Option<&str> {
        self.fixture
            .create_failures
            .get(project_id)
            .map(String::as_str)
    }
}

impl SessionsBackend for FakeSessions {
    fn list(&self) -> anyhow::Result<SessionsSnapshot> {
        let elapsed = match &self.clock {
            Clock::Manual(clock) => clock.elapsed(),
            Clock::Real(start) => start.elapsed(),
        }
        .as_millis();
        Ok(SessionsSnapshot {
            sessions: self
                .fixture
                .sessions
                .iter()
                .filter(|entry| u128::from(entry.appear_after_ms) <= elapsed)
                .map(|entry| entry.session.clone())
                .collect(),
        })
    }
}
