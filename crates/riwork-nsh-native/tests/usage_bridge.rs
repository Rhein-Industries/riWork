//! Bridge tests for `Usage` and `Accounts` (PANEL-064..066, PANEL-143..149,
//! PANEL-180..190, SHELL-221..224). Fake backends count every read that would
//! start a process; nothing here starts codex, grok or Orca, or reads a real
//! home.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier, Condvar, Mutex,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use riwork_core::codex_accounts::{self, AccountsSnapshot, CodexAccount, ForgottenAccounts, SYSTEM_DEFAULT_ID};
use riwork_engine::usage::{ProviderUsage, UsageWindow};
use riwork_nsh_native::{
    dto::ShellSessionDto,
    hub::{Snapshot, TopicSubscription},
    usage::{
        Accounts, CodexSettings, CodexTarget, CoreUsageBackend, FIXTURE_UNAVAILABLE,
        FixtureUsageBackend, STALE_AFTER_SECS, SystemClock, UNAVAILABLE_ACCOUNT, UsageBackend,
        UsageClock, UsageService, install_usage_service,
    },
};

const START: u64 = 1_800_000_000;

/// A gate a fake read waits on while closed.
#[derive(Default)]
struct Gate {
    closed: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn close(&self) {
        *self.closed.lock().unwrap() = true;
    }
    fn open(&self) {
        *self.closed.lock().unwrap() = false;
        self.changed.notify_all();
    }
    fn pass(&self) {
        let mut closed = self.closed.lock().unwrap();
        while *closed {
            closed = self.changed.wait(closed).unwrap();
        }
    }
}

#[derive(Default)]
struct Fake {
    reads: Mutex<BTreeMap<PathBuf, u32>>,
    results: Mutex<BTreeMap<PathBuf, Result<ProviderUsage, String>>>,
    read_gate: Gate,
    recorded: AtomicU32,
    discovers: AtomicU32,
    discover_gate: Gate,
    discovered: Mutex<Option<AccountsSnapshot>>,
    saved: Mutex<Option<AccountsSnapshot>>,
    forgotten: Mutex<Option<Result<ForgottenAccounts, String>>>,
    settings: Mutex<CodexSettings>,
    saves: AtomicU32,
    claude: Mutex<BTreeMap<String, ProviderUsage>>,
    grok_reads: AtomicU32,
}

impl Fake {
    fn reads(&self) -> u32 {
        self.reads.lock().unwrap().values().sum()
    }
    fn reads_of(&self, home: &str) -> u32 {
        self.reads
            .lock()
            .unwrap()
            .get(Path::new(home))
            .copied()
            .unwrap_or(0)
    }
    fn answer(&self, home: &str, result: Result<ProviderUsage, String>) {
        self.results.lock().unwrap().insert(home.into(), result);
    }
}

impl UsageBackend for Fake {
    fn read_codex(&self, target: &CodexTarget) -> Result<ProviderUsage, String> {
        *self
            .reads
            .lock()
            .unwrap()
            .entry(target.home.clone())
            .or_default() += 1;
        self.read_gate.pass();
        self.results
            .lock()
            .unwrap()
            .get(&target.home)
            .cloned()
            .unwrap_or_else(|| Err("Codex is signed out".into()))
    }
    fn record_codex(&self, _home: &Path, _usage: Option<&ProviderUsage>) {
        self.recorded.fetch_add(1, Ordering::SeqCst);
    }
    fn read_claude(&self, shell_id: &str) -> Result<Option<ProviderUsage>, String> {
        Ok(self.claude.lock().unwrap().get(shell_id).cloned())
    }
    fn read_grok(
        &self,
        _ids: &[String],
        _previous: &BTreeMap<String, riwork_engine::usage::GrokTabUsage>,
        _force: bool,
    ) -> BTreeMap<String, riwork_engine::usage::GrokTabUsage> {
        self.grok_reads.fetch_add(1, Ordering::SeqCst);
        BTreeMap::new()
    }
    fn cached_accounts(&self) -> Option<AccountsSnapshot> {
        self.saved.lock().unwrap().clone()
    }
    fn forgotten_accounts(&self) -> Result<ForgottenAccounts, String> {
        self.forgotten.lock().unwrap().clone().unwrap_or_else(|| Ok(ForgottenAccounts::default()))
    }
    fn discover_accounts(&self) -> AccountsSnapshot {
        self.discovers.fetch_add(1, Ordering::SeqCst);
        self.discover_gate.pass();
        self.discovered.lock().unwrap().clone().unwrap_or_default()
    }
    fn system_account(&self) -> Result<CodexAccount, String> {
        Ok(account(
            SYSTEM_DEFAULT_ID,
            "System default",
            "/fake/system",
            true,
        ))
    }
    fn codex_settings(&self) -> Result<CodexSettings, String> {
        Ok(self.settings.lock().unwrap().clone())
    }
    fn save_selected_account(&self, selected: Option<String>) -> Result<CodexSettings, String> {
        self.saves.fetch_add(1, Ordering::SeqCst);
        let mut settings = self.settings.lock().unwrap();
        settings.selected = selected;
        Ok(settings.clone())
    }
}

struct Clock(AtomicU64);

impl Clock {
    fn advance(&self, seconds: u64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl UsageClock for Clock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn account(id: &str, label: &str, home: &str, available: bool) -> CodexAccount {
    CodexAccount {
        id: id.into(),
        label: label.into(),
        email: label.contains('@').then(|| label.into()),
        home: home.into(),
        available,
        unavailable_reason: (!available).then(|| "This saved account's home is missing.".into()),
        is_system_default: id == SYSTEM_DEFAULT_ID,
    }
}

/// The system default, two saved accounts sharing one home, and one gone.
fn saved_list() -> AccountsSnapshot {
    AccountsSnapshot {
        accounts: vec![
            account(SYSTEM_DEFAULT_ID, "System default", "/fake/system", true),
            account("account-a", "a@example.test · Team A", "/fake/a", true),
            account("account-dup", "dup@example.test", "/fake/a", true),
            account("account-c", "c@example.test", "/fake/c", false),
        ],
        source_active_id: Some("account-a".into()),
        ..Default::default()
    }
}

fn quota(updated: u64) -> ProviderUsage {
    ProviderUsage {
        provider: "codex".into(),
        windows: vec![
            UsageWindow {
                label: "5h".into(),
                used_percent: 20.0,
                resets_at: None,
                window_minutes: Some(300),
            },
            UsageWindow {
                label: "7d".into(),
                used_percent: 40.0,
                resets_at: None,
                window_minutes: Some(10080),
            },
        ],
        updated_at_unix: updated,
        account_label: Some("sys@example.test · plus".into()),
        context_used_percent: None,
        session_cost_usd: None,
        session: None,
        session_error: None,
    }
}

fn shell(fields: serde_json::Value) -> ShellSessionDto {
    let mut base = serde_json::json!({
        "userOpened": true, "parentId": null, "projectId": "project", "worktreeId": null,
        "kind": "project", "cwd": "/w", "command": null, "editorPath": null, "harness": null,
        "unrestricted": false, "codexAccountId": null, "codexAccountLabel": null,
        "codexAccountEmail": null, "codexHome": null, "orchestratorSkillLoaded": false,
        "orchestratorSkillVersion": null, "orchestratorDelegationVersion": null,
        "orchestratorProjectRoot": null, "createdAtUnix": 1, "alive": true
    });
    for (key, value) in fields.as_object().unwrap() {
        base[key] = value.clone();
    }
    serde_json::from_value(base).unwrap()
}

fn start(fake: &Arc<Fake>, clock: Arc<dyn UsageClock>) -> Arc<UsageService> {
    start_with_shells(fake, clock, Vec::new())
}

fn start_with_shells(
    fake: &Arc<Fake>,
    clock: Arc<dyn UsageClock>,
    shells: Vec<ShellSessionDto>,
) -> Arc<UsageService> {
    let mut shells = Some(shells);
    UsageService::start(
        fake.clone(),
        clock,
        Box::new(move || shells.take().filter(|shells| !shells.is_empty())),
    )
    .unwrap()
}

fn manual() -> Arc<Clock> {
    Arc::new(Clock(AtomicU64::new(START)))
}

fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// Long enough for a woken service to have acted on a wake it should ignore.
fn settle() {
    thread::sleep(Duration::from_millis(250));
}

async fn until<T: Clone + 'static>(
    subscription: &TopicSubscription<T>,
    accept: impl Fn(&T) -> bool,
) -> Snapshot<T> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let snapshot = tokio::time::timeout(left, subscription.recv())
            .await
            .expect("timed out waiting for a matching snapshot")
            .expect("subscription open");
        if accept(&snapshot.data) {
            return snapshot;
        }
    }
}

#[tokio::test]
async fn usage_reads_nothing_without_a_subscriber_and_accounts_alone_read_no_usage() {
    let fake = Arc::new(Fake::default());
    let service = start(&fake, manual());
    service.wake();
    service.refresh_usage(true);
    settle();
    assert_eq!(fake.reads(), 0, "no subscriber, no Codex read");
    assert_eq!(fake.discovers.load(Ordering::SeqCst), 0);

    // Accounts alone are cache-only: they start no usage read either.
    let accounts = service.subscribe_accounts();
    let first = until(&accounts, |_| true).await;
    assert!(!first.data.listed);
    assert_eq!(first.data.accounts.len(), 1);
    settle();
    assert_eq!(fake.reads(), 0);

    let usage = service.subscribe_usage();
    until(&usage, |data| data.codex.len() == 1).await;
    wait_until("the system default's read", || fake.reads() == 1);
    drop(usage);
    service.refresh_usage(true);
    settle();
    assert_eq!(fake.reads(), 1, "a closed subscription stops reads");
    assert_eq!(fake.discovers.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn usage_reads_each_home_once_under_concurrent_subscribers_and_records_balancing() {
    let fake = Arc::new(Fake::default());
    *fake.saved.lock().unwrap() = Some(saved_list());
    fake.answer("/fake/system", Ok(quota(START)));
    fake.answer("/fake/a", Ok(quota(START)));
    fake.read_gate.close();
    let service = start(&fake, manual());
    let barrier = Arc::new(Barrier::new(8));
    let subscribers: Vec<_> = (0..8)
        .map(|_| {
            let service = service.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                service.subscribe_usage()
            })
        })
        .collect();
    let subscriptions: Vec<_> = subscribers
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    wait_until("both homes in flight", || fake.reads() == 2);
    // Refresh clicks coalesce while pending; no parallel read of one home.
    for _ in 0..3 {
        service.refresh_usage(true);
    }
    let pending = until(&subscriptions[0], |data| data.refreshing).await;
    assert_eq!(
        pending
            .data
            .codex
            .iter()
            .map(|entry| (entry.home.as_str(), entry.label.as_str(), entry.pending))
            .collect::<Vec<_>>(),
        [
            ("/fake/system", "System default", true),
            ("/fake/a", "a@example.test · Team A", true),
        ],
        "deduplicated by home; the unavailable account is not read"
    );
    settle();
    fake.read_gate.open();
    for subscription in &subscriptions {
        let done = until(subscription, |data| {
            !data.refreshing && data.codex.iter().all(|entry| entry.usage.is_some())
        })
        .await;
        assert!(!done.data.refreshing);
    }
    assert_eq!(fake.reads_of("/fake/system"), 2, "one queued refresh per home");
    assert_eq!(fake.reads_of("/fake/a"), 2);
    assert_eq!(fake.recorded.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn usage_backoff_follows_todays_intervals_on_the_service_clock() {
    let fake = Arc::new(Fake::default());
    let clock = manual();
    let service = start(&fake, clock.clone());
    let usage = service.subscribe_usage();
    let failed = until(&usage, |data| data.codex[0].failures == 1).await;
    assert_eq!(
        failed.data.codex[0].error.as_deref(),
        Some("Codex is signed out")
    );
    assert_eq!(failed.data.codex[0].last_attempt, Some(START));
    assert!(failed.data.codex[0].usage.is_none());

    // No figures, one failure: 60 s doubled.
    clock.advance(119);
    service.wake();
    settle();
    assert_eq!(fake.reads(), 1);
    clock.advance(1);
    service.wake();
    until(&usage, |data| data.codex[0].failures == 2).await;
    assert_eq!(fake.reads(), 2);

    // Two failures: 240 s. Then a success resets to the 15-minute interval.
    fake.answer("/fake/system", Ok(quota(START + 360)));
    clock.advance(239);
    service.wake();
    settle();
    assert_eq!(fake.reads(), 2);
    clock.advance(1);
    service.wake();
    let read = until(&usage, |data| data.codex[0].usage.is_some()).await;
    assert_eq!(read.data.codex[0].failures, 0);
    assert_eq!(fake.reads(), 3);
    clock.advance(15 * 60 - 1);
    service.wake();
    settle();
    assert_eq!(fake.reads(), 3);
    clock.advance(1);
    service.wake();
    wait_until("the 15-minute read", || fake.reads() == 4);

    // A forced refresh ignores the interval.
    until(&usage, |data| !data.refreshing).await;
    service.refresh_usage(true);
    wait_until("the forced read", || fake.reads() == 5);
}

#[tokio::test]
async fn usage_republishes_when_figures_turn_stale_without_any_other_wake() {
    let fake = Arc::new(Fake::default());
    let now = SystemClock.now();
    fake.answer("/fake/system", Ok(quota(now - STALE_AFTER_SECS + 1)));
    let service = start(&fake, Arc::new(SystemClock));
    let usage = service.subscribe_usage();
    let fresh = until(&usage, |data| data.codex[0].usage.is_some()).await;
    assert!(!fresh.data.codex[0].stale);
    let stale = until(&usage, |data| data.codex[0].stale).await;
    assert!(stale.data.now > fresh.data.now);
    assert_eq!(fake.reads(), 1, "staleness alone starts no read");
}

#[tokio::test]
async fn usage_refreshes_at_a_reset_without_another_wake_and_keeps_cached_figures_stale() {
    let fake = Arc::new(Fake::default());
    let now = SystemClock.now();
    let mut before = quota(now);
    before.windows[0].resets_at = Some(now + 2);
    fake.answer("/fake/system", Ok(before));
    let service = start(&fake, Arc::new(SystemClock));
    let usage = service.subscribe_usage();
    let fresh = until(&usage, |data| data.codex[0].usage.is_some() && !data.refreshing).await;
    assert!(!fresh.data.codex[0].stale);

    fake.read_gate.close();
    let reset = until(&usage, |data| data.codex[0].stale && data.refreshing).await;
    wait_until("reset-triggered read", || fake.reads() == 2);
    assert_eq!(reset.data.codex[0].usage.as_ref().unwrap().windows[0].used_percent, 20.0,
        "a reset never fabricates a new allowance");

    let mut after = quota(now + 2);
    after.windows[0].used_percent = 3.0;
    after.windows[0].resets_at = Some(now + 3600);
    fake.answer("/fake/system", Ok(after));
    fake.read_gate.open();
    let refreshed = until(&usage, |data| !data.refreshing && !data.codex[0].stale).await;
    assert_eq!(refreshed.data.codex[0].usage.as_ref().unwrap().windows[0].used_percent, 3.0);
    assert_eq!(fake.reads(), 2);
}

#[tokio::test]
async fn usage_retries_an_expired_server_window_with_backoff_and_no_busy_loop() {
    let fake = Arc::new(Fake::default());
    let clock = manual();
    let mut report = quota(START);
    report.windows[0].resets_at = Some(START + 10);
    fake.answer("/fake/system", Ok(report));
    let service = start(&fake, clock.clone());
    let usage = service.subscribe_usage();
    until(&usage, |data| data.codex[0].usage.is_some() && !data.refreshing).await;

    clock.advance(10);
    service.wake();
    until(&usage, |data| data.codex[0].stale && !data.refreshing).await;
    assert_eq!(fake.reads(), 2);
    for _ in 0..3 { service.wake(); }
    clock.advance(59);
    service.wake();
    settle();
    assert_eq!(fake.reads(), 2, "the same expired window cannot trigger a read loop");

    fake.answer("/fake/system", Err("Temporarily unavailable".into()));
    clock.advance(1);
    service.wake();
    until(&usage, |data| data.codex[0].failures == 1).await;
    assert_eq!(fake.reads(), 3);
    clock.advance(119);
    service.wake();
    settle();
    assert_eq!(fake.reads(), 3);
    fake.answer("/fake/system", Ok(quota(START + 190)));
    clock.advance(1);
    service.wake();
    until(&usage, |data| data.codex[0].failures == 0 && !data.codex[0].stale && !data.refreshing).await;
    assert_eq!(fake.reads(), 4);
}

#[tokio::test]
async fn usage_clock_rollback_does_not_delay_reads_until_the_old_clock_catches_up() {
    let fake = Arc::new(Fake::default());
    let clock = manual();
    fake.answer("/fake/system", Ok(quota(START)));
    let service = start(&fake, clock.clone());
    let usage = service.subscribe_usage();
    until(&usage, |data| data.codex[0].usage.is_some() && !data.refreshing).await;
    clock.0.store(START - 3600, Ordering::SeqCst);
    fake.answer("/fake/system", Ok(quota(START - 3600)));
    service.wake();
    until(&usage, |data| data.codex[0].last_attempt == Some(START - 3600) && !data.refreshing).await;
    assert_eq!(fake.reads(), 2);
}

#[tokio::test]
async fn usage_reads_claude_caches_of_listed_shells_and_live_codex_shell_homes() {
    let fake = Arc::new(Fake::default());
    fake.claude
        .lock()
        .unwrap()
        .insert("claude-1".into(), quota(START));
    let service = start_with_shells(
        &fake,
        manual(),
        vec![
            shell(serde_json::json!({"id": "claude-1", "harness": "claude"})),
            shell(serde_json::json!({"id": "claude-2", "harness": "claude"})),
            shell(serde_json::json!({"id": "codex-1", "harness": "codex",
                "codexHome": "/fake/session", "codexAccountId": "old",
                "codexAccountLabel": "old@example.test · Old"})),
            shell(serde_json::json!({"id": "grok-1", "harness": "grok"})),
        ],
    );
    let usage = service.subscribe_usage();
    let data = until(&usage, |data| {
        data.claude.len() == 2 && data.codex.len() == 2
    })
    .await
    .data;
    assert_eq!(
        data.claude
            .iter()
            .map(|entry| (
                entry.shell_id.as_str(),
                entry.waiting,
                entry.usage.is_some()
            ))
            .collect::<Vec<_>>(),
        [("claude-1", false, true), ("claude-2", true, false)]
    );
    assert_eq!(data.codex[1].home, "/fake/session");
    assert_eq!(data.codex[1].label, "old@example.test · Old");
    assert_eq!(data.grok.len(), 1, "live Grok shells get a placeholder");
    assert_eq!(data.grok[0].shell_id, "grok-1");
    assert!(data.grok[0].usage.is_none() && !data.grok[0].pending);
    wait_until("both Codex homes", || fake.reads() == 2);
    assert_eq!(fake.grok_reads.load(Ordering::SeqCst), 0,
        "Grok is not read without a visible usage panel");
}

#[tokio::test]
async fn accounts_subscribe_is_cache_only_and_refresh_is_single_flight() {
    let fake = Arc::new(Fake::default());
    *fake.saved.lock().unwrap() = Some(saved_list());
    let mut discovered = saved_list();
    discovered.error =
        Some("Orca's CLI is unavailable. Showing previously discovered accounts.".into());
    discovered.from_cache = true;
    *fake.discovered.lock().unwrap() = Some(discovered);
    let service = start(&fake, manual());
    let accounts = service.subscribe_accounts();
    let first = until(&accounts, |_| true).await.data;
    assert!(first.listed && !first.refreshing && !first.from_cache);
    assert_eq!(
        first
            .accounts
            .iter()
            .map(|account| (
                account.id.as_str(),
                account.detail.as_str(),
                account.available
            ))
            .collect::<Vec<_>>(),
        [
            (
                SYSTEM_DEFAULT_ID,
                "Current Codex profile · /fake/system",
                true
            ),
            ("account-a", "Current Orca account", true),
            ("account-dup", "Saved in Orca", true),
            ("account-c", "This saved account's home is missing.", false),
        ]
    );
    settle();
    assert_eq!(
        fake.discovers.load(Ordering::SeqCst),
        0,
        "subscribe never discovers"
    );

    fake.discover_gate.close();
    let first_refresh = {
        let service = service.clone();
        thread::spawn(move || service.refresh_accounts())
    };
    wait_until("the discovery", || {
        fake.discovers.load(Ordering::SeqCst) == 1
    });
    until(&accounts, |data| data.refreshing).await;
    let second_refresh = {
        let service = service.clone();
        thread::spawn(move || service.refresh_accounts())
    };
    settle();
    fake.discover_gate.open();
    let (a, b) = (
        first_refresh.join().unwrap(),
        second_refresh.join().unwrap(),
    );
    assert_eq!(fake.discovers.load(Ordering::SeqCst), 1, "single-flight");
    assert_eq!(a, b);
    assert!(a.from_cache && !a.refreshing);
    assert_eq!(
        a.error.as_deref(),
        Some("Orca's CLI is unavailable. Showing previously discovered accounts.")
    );
    until(&accounts, |data| data.from_cache && !data.refreshing).await;

    // A later cache update supersedes discovery, even though this service has
    // already discovered once. Unchanged cache reads above retain its error.
    *fake.saved.lock().unwrap() = Some(AccountsSnapshot {
        accounts: vec![account("account-new", "New account", "/fake/new", true)],
        source_active_id: Some("account-new".into()),
        ..Default::default()
    });
    service.wake();
    let changed = until(&accounts, |data| data.accounts.iter().any(|account| account.id == "account-new")).await;
    assert!(!changed.data.from_cache);
    assert!(changed.data.error.is_none());
    assert_eq!(changed.data.accounts.len(), 1);
    assert!(changed.data.accounts[0].orca_active);
    assert!(service.select_default(Some("account-a".into())).is_err());
    assert_eq!(fake.discovers.load(Ordering::SeqCst), 1, "cache reconciliation never discovers");
    assert_eq!(fake.reads(), 0, "accounts alone still start no quota reads");
}

#[tokio::test]
async fn accounts_select_refuses_unavailable_and_saves_the_system_default_as_null() {
    let fake = Arc::new(Fake::default());
    *fake.saved.lock().unwrap() = Some(saved_list());
    let service = start(&fake, manual());
    for id in ["account-c", "removed-account"] {
        let refused = service.select_default(Some(id.into())).unwrap_err();
        assert_eq!(refused, format!("refused: {UNAVAILABLE_ACCOUNT}"));
    }
    assert_eq!(
        fake.saves.load(Ordering::SeqCst),
        0,
        "a refusal changes nothing"
    );
    let chosen = service.select_default(Some("account-a".into())).unwrap();
    assert_eq!(chosen.selected_id.as_deref(), Some("account-a"));
    assert!(!chosen.selected_unavailable);
    assert_eq!(
        fake.settings.lock().unwrap().selected.as_deref(),
        Some("account-a")
    );
    for system in [None, Some(SYSTEM_DEFAULT_ID.to_owned())] {
        fake.settings.lock().unwrap().selected = Some("account-a".into());
        let saved = service.select_default(system).unwrap();
        assert_eq!(saved.selected_id, None);
        assert_eq!(fake.settings.lock().unwrap().selected, None);
    }

    // The JS surface rejects with the same coded error.
    install_usage_service(service.clone()).unwrap();
    let refused = Accounts::select_default(Some("account-c".into()))
        .await
        .unwrap_err();
    assert!(refused.starts_with("refused: "), "{refused}");
}

#[test]
fn fixture_backend_reads_canned_figures_and_starts_no_process() {
    let home = tempfile::tempdir().unwrap();
    let backend = FixtureUsageBackend::new(home.path().to_owned()).unwrap();
    let system = CodexTarget {
        home: "/fixture/codex".into(),
        account_id: Some(SYSTEM_DEFAULT_ID.into()),
        label: "System default".into(),
    };
    assert_eq!(
        backend.read_codex(&system).unwrap_err(),
        FIXTURE_UNAVAILABLE
    );
    std::fs::write(
        home.path().join("usage-fixture.json"),
        serde_json::json!({ "codex": {
            "system-default": {
                "provider": "codex", "updated_at_unix": 0, "account_label": "sys@example.test",
                "windows": [{ "label": "5h", "used_percent": 20.0, "resets_at": null, "window_minutes": 300 }]
            },
            "account-a": { "error": "Codex is signed out" }
        }})
        .to_string(),
    )
    .unwrap();
    let usage = backend.read_codex(&system).unwrap();
    assert_eq!(usage.account_label.as_deref(), Some("sys@example.test"));
    assert!(usage.updated_at_unix > START - 10 * 365 * 86400);
    let signed_out = CodexTarget {
        account_id: Some("account-a".into()),
        ..system.clone()
    };
    assert_eq!(
        backend.read_codex(&signed_out).unwrap_err(),
        "Codex is signed out"
    );
    // Discovery never consults Orca; with no saved list it says so.
    let discovered = backend.discover_accounts();
    assert!(!discovered.from_cache);
    assert_eq!(
        discovered.error.as_deref(),
        Some("Orca is not consulted in this fixture.")
    );
    assert!(backend.cached_accounts().is_none());
    // Claude's status-line cache is a file in the fixture home.
    let shell = "6f9619ff-8b86-d011-b42d-00c04fc964ff";
    assert_eq!(backend.read_claude(shell).unwrap(), None);
    std::fs::create_dir_all(home.path().join("usage")).unwrap();
    std::fs::write(
        home.path().join("usage").join(format!("{shell}.json")),
        serde_json::json!({ "provider": "claude", "updated_at_unix": START, "account_label": null,
            "windows": [{ "label": "5h", "used_percent": 50.0, "resets_at": null, "window_minutes": 300 }] })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        backend.read_claude(shell).unwrap().unwrap().windows[0].used_percent,
        50.0
    );
}

#[test]
fn core_backend_settings_and_saved_list_are_file_only() {
    let home = tempfile::tempdir().unwrap();
    let backend = CoreUsageBackend::new(home.path().to_owned()).unwrap();
    assert!(backend.cached_accounts().is_none());
    assert_eq!(backend.codex_settings().unwrap(), CodexSettings::default());
    std::fs::write(
        home.path().join("settings.json"),
        r#"{"codex_subscription_balancing":true,"future_key":{"kept":true}}"#,
    )
    .unwrap();
    let saved = backend
        .save_selected_account(Some("account-a".into()))
        .unwrap();
    assert_eq!(
        saved,
        CodexSettings {
            selected: Some("account-a".into()),
            balancing: true
        }
    );
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.path().join("settings.json")).unwrap()).unwrap();
    assert_eq!(file["selected_codex_account"], "account-a");
    assert_eq!(file["future_key"]["kept"], true);
    backend.save_selected_account(None).unwrap();
    assert_eq!(backend.codex_settings().unwrap().selected, None);
}

#[tokio::test]
async fn forgotten_accounts_stay_hidden_across_cached_discovery_and_live_shell_fallbacks() {
    const TEST: &str = "forgotten_accounts_stay_hidden_across_cached_discovery_and_live_shell_fallbacks";
    const CHILD: &str = "RIWORK_TEST_FORGOTTEN_ACCOUNTS_HOME";
    const OLD: &str = "2c47d98a-9500-4df2-90ec-a96c0352b682";
    const KEPT: &str = "e120aac3-6a5a-48fd-a0ea-7613013cef5b";
    let Some(root) = std::env::var_os(CHILD).map(PathBuf::from) else {
        // Isolate profile selection without touching the developer's homes or
        // changing process-wide environment alongside other bridge tests.
        let root = tempfile::tempdir().unwrap();
        for directory in ["profile", "system", "state"] {
            std::fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        let fixture_root = root.path().canonicalize().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, &fixture_root)
            .env("ORCA_USER_DATA_PATH", fixture_root.join("profile"))
            .env("CODEX_HOME", fixture_root.join("system"))
            .env_remove("RIWORK_CODEX_ACCOUNT_HOME")
            .output()
            .unwrap();
        assert!(output.status.success(), "{}\n{}",
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    };
    let root = root.canonicalize().unwrap();
    let state_home = root.join("state");
    let profile = root.join("profile");
    let old_home = profile.join("codex-accounts").join(OLD).join("home");
    let kept_home = profile.join("codex-accounts").join(KEPT).join("home");
    let unlisted_home = root.join("unlisted-live-home");
    for home in [&old_home, &kept_home, &unlisted_home] {
        std::fs::create_dir_all(home).unwrap();
    }
    let retained = old_home.join("conversation-sentinel.jsonl");
    std::fs::write(&retained, "retained live conversation\n").unwrap();
    let cache = serde_json::json!({"version":1,"user_data":profile,"source_active_id":OLD,
        "accounts":[
            {"id":OLD,"email":"old@example.test","managedHomeRuntime":"host"},
            {"id":KEPT,"email":"kept@example.test","managedHomeRuntime":"host"}
        ]}).to_string();
    let cache_path = state_home.join("codex-accounts.json");
    std::fs::write(&cache_path, &cache).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cache_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let backend = CoreUsageBackend::new(state_home.clone()).unwrap();
    let original = backend.cached_accounts().unwrap();
    assert!(original.accounts.iter().any(|account| account.id == OLD));
    assert!(!backend.forgotten_accounts().unwrap().hides(Some(OLD), &old_home));

    let fake = Arc::new(Fake::default());
    *fake.saved.lock().unwrap() = Some(original.clone());
    *fake.discovered.lock().unwrap() = Some(original);
    let service = start_with_shells(&fake, manual(), vec![
        shell(serde_json::json!({"id":"old-id","harness":"codex","codexHome":old_home,
            "codexAccountId":OLD,"codexAccountLabel":"Frozen old label"})),
        // Even a missing catalog ID must not recreate the forgotten home card.
        shell(serde_json::json!({"id":"old-home-only","harness":"codex","codexHome":old_home})),
        shell(serde_json::json!({"id":"unlisted","harness":"codex","codexHome":unlisted_home})),
    ]);
    let usage = service.subscribe_usage();
    let accounts = service.subscribe_accounts();
    service.refresh_accounts();
    until(&usage, |data| data.codex.len() == 4 && !data.refreshing).await;
    let old_reads = fake.reads_of(old_home.to_str().unwrap());
    assert_eq!(old_reads, 1);

    codex_accounts::forget_account(&state_home, OLD).unwrap();
    // The warm production backend notices policy-only changes even though raw
    // discovery metadata remains byte-for-byte intact for idempotent lookup.
    assert_eq!(std::fs::read_to_string(&cache_path).unwrap(), cache);
    assert!(!backend.cached_accounts().unwrap().accounts.iter().any(|account| account.id == OLD));
    let policy = backend.forgotten_accounts().unwrap();
    assert!(policy.hides(None, &old_home));
    *fake.forgotten.lock().unwrap() = Some(Ok(policy));
    service.wake();
    until(&accounts, |data| data.accounts.iter().all(|account| account.id != OLD)).await;
    until(&usage, |data| data.codex.len() == 3
        && data.codex.iter().all(|entry| entry.home != old_home.to_string_lossy())).await;
    // A cached discovery can still contain the account; it must remain hidden.
    assert!(service.refresh_accounts().accounts.iter().all(|account| account.id != OLD));
    assert!(service.select_default(Some(OLD.into())).is_err());
    service.refresh_usage(true);
    settle();
    assert_eq!(fake.reads_of(old_home.to_str().unwrap()), old_reads,
        "forgotten live homes must not be polled again");
    let target = CodexTarget { home: old_home.clone(), account_id: Some(OLD.into()), label: "old".into() };
    assert!(backend.read_codex(&target).unwrap_err().contains("forgotten"),
        "a queued forgotten target must be refused before spawning Codex");
    assert_eq!(std::fs::read_to_string(&retained).unwrap(), "retained live conversation\n");

    *fake.forgotten.lock().unwrap() = Some(Err("Unreadable account selection policy".into()));
    service.wake();
    until(&accounts, |data| data.accounts.len() == 1
        && data.accounts[0].system && data.error.as_deref() == Some("Unreadable account selection policy")).await;
    until(&usage, |data| data.codex.len() == 1
        && data.codex[0].account_id.as_deref() == Some(SYSTEM_DEFAULT_ID)).await;

    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let policy_path = state_home.join(codex_accounts::FORGOTTEN_ACCOUNTS_FILE_NAME);
        // chmod leaves mtime/length unchanged; trust changes still invalidate
        // both the policy cache and the previously filtered catalog cache.
        std::fs::set_permissions(&policy_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(backend.forgotten_accounts().is_err());
        assert!(backend.cached_accounts().is_none());
        std::fs::set_permissions(&policy_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(backend.forgotten_accounts().unwrap().hides(None, &old_home));
        let replacement = state_home.join("retained-policy.json");
        std::fs::rename(&policy_path, &replacement).unwrap();
        symlink(&replacement, &policy_path).unwrap();
        assert!(backend.forgotten_accounts().is_err());
        assert!(backend.cached_accounts().is_none());
    }
}
