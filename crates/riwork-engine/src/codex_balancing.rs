//! Account choice for new automatic Codex launches. Quota is read through Codex's
//! public app-server, never through credentials or a conversation. Explicit account
//! choices and resumes bypass this module's balancing policy.

use crate::{
    codex_accounts::{self, CodexAccountBinding},
    settings::SettingsStore,
    store::{ProjectCodexAccount, Store},
    usage::{self, ProviderUsage},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const CACHE_DIRECTORY: &str = "codex-subscription-usage";
const FRESH_SECONDS: u64 = 120;
const REFRESH_SECONDS: u64 = 60;
const FAILURE_RETRY_SECONDS: u64 = 15;
const REFRESH_BUDGET: Duration = Duration::from_secs(10);
/// A read is not started with less of the shared budget left: it would time out, kill a
/// Codex app-server that just started and replace a usable sample with a failure.
const MIN_READ_BUDGET: Duration = Duration::from_secs(2);
const PARALLEL_READS: usize = 4;
const MAX_SAMPLE_BYTES: u64 = 128 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Sample {
    home: PathBuf,
    attempted_at: u64,
    // An unsuccessful latest read invalidates the old quota for routing. The UI may
    // still display old numbers, but they must not choose a new account.
    usage: Option<ProviderUsage>,
}

/// A local preview of a launch choice, for catalogs and plain-shell defaults.
/// It never starts a provider process. A real launch refreshes before choosing.
pub fn cached_binding(
    state_home: &Path,
    project_id: Option<&str>,
) -> Result<CodexAccountBinding, String> {
    resolve(state_home, project_id, false)
}

/// Whether balancing chooses a new automatic session's account for `project_id`: the
/// setting is on and the project, if any, inherits the app's account. A preview of such a
/// choice must not be pinned; the launch chooses again with fresh quota.
pub fn balances(state_home: &Path, project_id: Option<&str>) -> bool {
    let inherits = project_id.is_none_or(|project_id| {
        Store::open(state_home)
            .and_then(|store| store.snapshot())
            .and_then(|snapshot| {
                snapshot
                    .project(project_id)
                    .map(|project| matches!(project.codex_account, ProjectCodexAccount::Inherit))
            })
            .unwrap_or(false)
    });
    inherits
        && SettingsStore::open(state_home)
            .and_then(|store| store.load())
            .is_ok_and(|settings| settings.codex_subscription_balancing)
}

/// Choose a new automatic session's account. Call on a worker: at most four quota
/// readers run at once under a shared ten-second deadline. No paid model call occurs.
pub fn new_binding(
    state_home: &Path,
    project_id: Option<&str>,
) -> Result<CodexAccountBinding, String> {
    resolve(state_home, project_id, true)
}

fn resolve(
    state_home: &Path,
    project_id: Option<&str>,
    refresh: bool,
) -> Result<CodexAccountBinding, String> {
    if let Some(project_id) = project_id {
        let project = Store::open(state_home)?
            .snapshot()?
            .project(project_id)?
            .clone();
        match project.codex_account {
            ProjectCodexAccount::Saved(id) => {
                return codex_accounts::resolve_launch_binding(state_home, Some(&id));
            }
            ProjectCodexAccount::SystemDefault => {
                return codex_accounts::resolve_launch_binding(state_home, None);
            }
            ProjectCodexAccount::Inherit => {}
        }
    }
    let settings = SettingsStore::open(state_home)?.load()?;
    let fallback = || {
        codex_accounts::resolve_launch_binding(
            state_home,
            settings.selected_codex_account.as_deref(),
        )
    };
    if !settings.codex_subscription_balancing {
        return fallback();
    }
    let mut accounts = codex_accounts::cached_accounts(state_home);
    if refresh && accounts.is_err() {
        // Account discovery reads only Orca's public metadata. Cached peeks must
        // never launch Orca (or create a cache) while a UI is rendering.
        codex_accounts::discover_in(state_home);
        accounts = codex_accounts::cached_accounts(state_home);
    }
    let candidates: Vec<_> = accounts
        .unwrap_or_default()
        .into_iter()
        .filter(|account| account.available && !account.is_system_default)
        .filter_map(|account| {
            codex_accounts::resolve_launch_binding(state_home, Some(&account.id)).ok()
        })
        .collect();
    if refresh && !candidates.is_empty() {
        // A sample that could not be saved only leaves its account unknown, as the
        // choice below reads them back; the other accounts are still chosen among.
        if let Err(error) = refresh_samples(state_home, &candidates) {
            eprintln!("riwork codex balancing: {error}");
        }
    }
    let now = unix_now();
    let mut scored = Vec::new();
    let mut excluded = Vec::new();
    for binding in candidates {
        let sample = read_sample(state_home, &binding.home);
        match sample
            .as_ref()
            .and_then(|sample| sample.usage.as_ref())
            .and_then(|usage| headroom(usage, now))
        {
            Some(headroom) => scored.push((binding, headroom)),
            None => excluded.push(format!(
                "{} ({})",
                binding
                    .label
                    .as_deref()
                    .or(binding.id.as_deref())
                    .unwrap_or("account"),
                exclusion(sample.as_ref(), now)
            )),
        }
    }
    if let Some(binding) = choose(&scored, settings.selected_codex_account.as_deref()) {
        return Ok(binding.clone());
    }
    if !refresh {
        // Catalogs can still show the configured account before the first launch;
        // the launch itself will refuse unknown quota instead of guessing.
        return fallback();
    }
    Err(no_account(&excluded))
}

/// Why balancing passed over an account, for the launch's error.
fn exclusion(sample: Option<&Sample>, now: u64) -> &'static str {
    let Some(sample) = sample else {
        return "quota not read in time";
    };
    let Some(usage) = &sample.usage else {
        return "quota read failed";
    };
    if usage.updated_at_unix > now || now.saturating_sub(usage.updated_at_unix) > FRESH_SECONDS {
        "quota report too old"
    } else if usage
        .windows
        .iter()
        .any(|window| window.resets_at.is_some_and(|reset| reset <= now))
    {
        "a limit reset since its report"
    } else if usage
        .windows
        .iter()
        .any(|window| window.used_percent >= 100.0)
    {
        "a limit is used up"
    } else {
        "quota report unusable"
    }
}

fn no_account(excluded: &[String]) -> String {
    if excluded.is_empty() {
        return "Codex subscription balancing found no saved Codex account to choose (the system default is not balanced). Refresh accounts in Settings > Codex accounts, name an account with --account, or turn off subscription balancing.".into();
    }
    format!(
        "Codex subscription balancing found no account with fresh, usable quota: {}. Retry, name an account with --account or pin the project's account, or turn off subscription balancing in Settings > Codex accounts.",
        excluded.join(", ")
    )
}

/// Maximize the tightest reported allowance. A large weekly allowance cannot hide
/// an exhausted short window (nor can a large short allowance hide a spent week).
fn headroom(usage: &ProviderUsage, now: u64) -> Option<f64> {
    if usage.provider != "codex"
        || usage.windows.is_empty()
        || usage.updated_at_unix > now
        || now.saturating_sub(usage.updated_at_unix) > FRESH_SECONDS
    {
        return None;
    }
    let mut remaining: f64 = 100.0;
    for window in &usage.windows {
        if !window.used_percent.is_finite()
            || !(0.0..100.0).contains(&window.used_percent)
            || window.resets_at.is_some_and(|reset| reset <= now)
        {
            return None;
        }
        remaining = remaining.min(100.0 - window.used_percent);
    }
    Some(remaining)
}

fn choose<'a>(
    candidates: &'a [(CodexAccountBinding, f64)],
    preferred: Option<&str>,
) -> Option<&'a CodexAccountBinding> {
    candidates
        .iter()
        .max_by(|(left, left_quota), (right, right_quota)| {
            left_quota
                .total_cmp(right_quota)
                .then_with(|| {
                    (left.id.as_deref() == preferred).cmp(&(right.id.as_deref() == preferred))
                })
                // Stable ties, independent of discovery/worker completion order.
                .then_with(|| right.id.cmp(&left.id))
        })
        .map(|(binding, _)| binding)
}

fn refresh_samples(state_home: &Path, candidates: &[CodexAccountBinding]) -> Result<(), String> {
    refresh_samples_with(state_home, candidates, REFRESH_BUDGET, |home, timeout| {
        usage::read_codex_usage_at_timeout(home, timeout)
    })
}

fn refresh_samples_with(
    state_home: &Path,
    candidates: &[CodexAccountBinding],
    budget: Duration,
    reader: impl Fn(&Path, Duration) -> Result<ProviderUsage, String> + Sync,
) -> Result<(), String> {
    let directory = cache_directory(state_home)?;
    let deadline = Instant::now() + budget;
    // Serialize refresh rounds across the CLI, app and chat host. The lock is
    // bounded too: simultaneous launches reuse the first launch's fresh samples.
    let lock = private_options()
        .create(true)
        .read(true)
        .write(true)
        .open(directory.join("refresh.lock"))
        .map_err(|error| format!("Cannot open Codex quota refresh lock: {error}"))?;
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    // No old sample is promoted to fresh by a busy reader.
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(format!("Cannot lock Codex quota refresh: {error}")),
        }
    }
    let now = unix_now();
    let pending = Mutex::new(
        candidates
            .iter()
            .filter(|binding| needs_refresh(read_sample(state_home, &binding.home).as_ref(), now))
            .map(|binding| binding.home.clone())
            .collect::<VecDeque<_>>(),
    );
    let errors = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for _ in 0..PARALLEL_READS.min(candidates.len()) {
            let pending = &pending;
            let errors = &errors;
            let reader = &reader;
            scope.spawn(move || {
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining < MIN_READ_BUDGET.min(budget / 5) {
                        break;
                    }
                    let home = pending.lock().unwrap().pop_front();
                    let Some(home) = home else { break };
                    let result = reader(&home, remaining);
                    // Cut short by the shared deadline rather than failed: the previous
                    // sample stays, and `headroom` still judges whether it is fresh.
                    if result.is_err() && Instant::now() >= deadline {
                        continue;
                    }
                    if let Err(error) = record_result(state_home, &home, result.as_ref().ok()) {
                        errors.lock().unwrap().push(error);
                    }
                }
            });
        }
    });
    drop(lock);
    match errors.into_inner().unwrap().into_iter().next() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn needs_refresh(sample: Option<&Sample>, now: u64) -> bool {
    let Some(sample) = sample else { return true };
    if sample.attempted_at > now {
        return true;
    }
    let age = now.saturating_sub(sample.attempted_at);
    match &sample.usage {
        // A limit that reset since its report may be usable again, so it is read soon; a
        // used-up one is not read on every launch until it resets.
        Some(usage) => {
            age >= REFRESH_SECONDS
                || (age >= FAILURE_RETRY_SECONDS
                    && usage
                        .windows
                        .iter()
                        .any(|window| window.resets_at.is_some_and(|reset| reset <= now)))
        }
        None => age >= FAILURE_RETRY_SECONDS,
    }
}

/// Feed a successful/failed public quota read into the same cache used by launches.
/// Only numeric usage and the account home are saved; no auth or server errors.
pub fn record_result(
    state_home: &Path,
    account_home: &Path,
    usage: Option<&ProviderUsage>,
) -> Result<(), String> {
    let directory = cache_directory(state_home)?;
    let sample = Sample {
        home: account_home.to_owned(),
        attempted_at: unix_now(),
        // The label is the account's email and plan; routing needs neither.
        usage: usage.cloned().map(|usage| ProviderUsage {
            account_label: None,
            ..usage
        }),
    };
    let path = sample_path(&directory, account_home);
    let temporary = directory.join(format!(".sample-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = private_options()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("Cannot create Codex quota cache: {error}"))?;
        serde_json::to_writer(&mut file, &sample)
            .map_err(|error| format!("Cannot encode Codex quota cache: {error}"))?;
        file.flush()
            .map_err(|error| format!("Cannot write Codex quota cache: {error}"))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("Cannot replace Codex quota cache: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn read_sample(state_home: &Path, home: &Path) -> Option<Sample> {
    let directory = state_home.join(CACHE_DIRECTORY);
    if !fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.is_dir()) {
        return None;
    }
    let path = sample_path(&directory, home);
    let file = private_options().read(true).open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SAMPLE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_SAMPLE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_SAMPLE_BYTES {
        return None;
    }
    let sample: Sample = serde_json::from_slice(&bytes).ok()?;
    (sample.home == home).then_some(sample)
}

fn sample_path(directory: &Path, home: &Path) -> PathBuf {
    // A stable, non-secret filename; verify the complete path after decoding so a
    // hash collision can never attach one account's allowance to another account.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in home.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    directory.join(format!("{hash:016x}.json"))
}

fn cache_directory(state_home: &Path) -> Result<PathBuf, String> {
    let directory = state_home.join(CACHE_DIRECTORY);
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("Cannot create Codex quota cache: {error}")),
    }
    if !fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.is_dir()) {
        return Err("Codex quota cache must be a real directory".into());
    }
    Ok(directory)
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::UsageWindow;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const NOW: u64 = 1_000_000;

    fn quota(used: [f64; 2], now: u64) -> ProviderUsage {
        ProviderUsage {
            provider: "codex".into(),
            windows: [300, 10_080]
                .into_iter()
                .zip(used)
                .map(|(minutes, used_percent)| UsageWindow {
                    label: minutes.to_string(),
                    used_percent,
                    resets_at: Some(now + minutes * 60),
                    window_minutes: Some(minutes),
                })
                .collect(),
            updated_at_unix: now,
            account_label: Some("subscription".into()),
            context_used_percent: None,
            session_cost_usd: None,
            session: None,
            session_error: None,
        }
    }

    fn binding(id: &str) -> CodexAccountBinding {
        CodexAccountBinding {
            id: Some(id.into()),
            label: Some(id.into()),
            email: None,
            home: PathBuf::from(format!("/accounts/{id}")),
        }
    }

    #[test]
    fn the_tightest_window_decides_and_app_preference_breaks_only_equal_scores() {
        let candidates: Vec<_> = [("a", [0.0, 85.0]), ("b", [50.0, 50.0]), ("c", [75.0, 0.0])]
            .into_iter()
            .map(|(id, used)| (binding(id), headroom(&quota(used, NOW), NOW).unwrap()))
            .collect();
        assert_eq!(
            choose(&candidates, Some("a")).unwrap().id.as_deref(),
            Some("b")
        );
        let ties = [
            (binding("c"), 50.0),
            (binding("a"), 50.0),
            (binding("b"), 50.0),
        ];
        assert_eq!(choose(&ties, Some("b")).unwrap().id.as_deref(), Some("b"));
        assert_eq!(
            choose(&ties, Some("missing")).unwrap().id.as_deref(),
            Some("a")
        );
        assert_eq!(choose(&ties, None).unwrap().id.as_deref(), Some("a"));
    }

    #[test]
    fn exhausted_invalid_stale_future_and_expired_allowances_never_qualify() {
        assert_eq!(headroom(&quota([0.0, 0.0], NOW), NOW), Some(100.0));
        for used in [
            [100.0, 0.0],
            [0.0, 100.0],
            [101.0, 0.0],
            [-1.0, 0.0],
            [f64::NAN, 0.0],
        ] {
            assert_eq!(headroom(&quota(used, NOW), NOW), None, "{used:?}");
        }
        assert_eq!(
            headroom(&quota([1.0, 2.0], NOW - FRESH_SECONDS - 1), NOW),
            None
        );
        assert_eq!(headroom(&quota([1.0, 2.0], NOW + 1), NOW), None);
        let mut expired = quota([1.0, 2.0], NOW);
        expired.windows[0].resets_at = Some(NOW);
        assert_eq!(
            headroom(&expired, NOW),
            None,
            "a reset is not assumed to restore allowance"
        );
        expired.windows.clear();
        assert_eq!(headroom(&expired, NOW), None);
        let mut wrong_provider = quota([1.0, 2.0], NOW);
        wrong_provider.provider = "claude".into();
        assert_eq!(headroom(&wrong_provider, NOW), None);
    }

    struct Fixture {
        root: PathBuf,
        state: PathBuf,
        accounts: Vec<CodexAccountBinding>,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("riwork-balancing-{}", Uuid::new_v4()));
            let state = root.join("state");
            fs::create_dir_all(&state).unwrap();
            Self {
                root,
                state,
                accounts: vec![binding("a"), binding("b")],
            }
        }

        fn sample(&self, index: usize, usage: Option<&ProviderUsage>) {
            record_result(&self.state, &self.accounts[index].home, usage).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn failed_reads_invalidate_good_old_quota_and_retry_after_a_short_backoff() {
        let fixture = Fixture::new();
        let now = unix_now();
        fixture.sample(0, Some(&quota([10.0, 20.0], now)));
        let first = read_sample(&fixture.state, &fixture.accounts[0].home).unwrap();
        // From the sample's own attempt time: a second may pass after `now`.
        assert!(!needs_refresh(Some(&first), first.attempted_at));
        assert!(needs_refresh(
            Some(&first),
            first.attempted_at + REFRESH_SECONDS
        ));
        fixture.sample(0, None);
        let failed = read_sample(&fixture.state, &fixture.accounts[0].home).unwrap();
        assert!(failed.usage.is_none());
        assert!(!needs_refresh(Some(&failed), failed.attempted_at));
        assert!(needs_refresh(
            Some(&failed),
            failed.attempted_at + FAILURE_RETRY_SECONDS
        ));
        assert!(read_sample(&fixture.state, &fixture.accounts[1].home).is_none());
    }

    #[test]
    fn refresh_is_bounded_parallel_and_warm_samples_are_shared_across_callers() {
        let mut fixture = Fixture::new();
        fixture
            .accounts
            .extend([binding("c"), binding("d"), binding("e"), binding("f")]);
        let calls = AtomicUsize::new(0);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let reader = |_: &Path, timeout: Duration| {
            assert!(timeout <= Duration::from_millis(250));
            calls.fetch_add(1, Ordering::SeqCst);
            let concurrency = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(concurrency, Ordering::SeqCst);
            thread::sleep(timeout.min(Duration::from_millis(25)));
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(quota([1.0, 2.0], unix_now()))
        };
        refresh_samples_with(
            &fixture.state,
            &fixture.accounts,
            Duration::from_millis(250),
            reader,
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 6);
        assert!((2..=PARALLEL_READS).contains(&peak.load(Ordering::SeqCst)));
        refresh_samples_with(
            &fixture.state,
            &fixture.accounts,
            Duration::from_millis(250),
            |_, _| panic!("another process/caller reuses the persisted fresh samples"),
        )
        .unwrap();
    }

    #[test]
    fn a_shared_deadline_stops_scheduling_more_accounts_after_slow_reads() {
        let mut fixture = Fixture::new();
        fixture
            .accounts
            .extend((0..20).map(|n| binding(&format!("more-{n}"))));
        let calls = AtomicUsize::new(0);
        let start = Instant::now();
        refresh_samples_with(
            &fixture.state,
            &fixture.accounts,
            Duration::from_millis(80),
            |_, timeout| {
                calls.fetch_add(1, Ordering::SeqCst);
                thread::sleep(timeout);
                Err("fixture timeout".into())
            },
        )
        .unwrap();
        assert!(calls.load(Ordering::SeqCst) <= PARALLEL_READS);
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_used_up_account_waits_for_its_reset_and_the_error_says_why_each_was_passed_over() {
        let fixture = Fixture::new();
        let now = unix_now();
        fixture.sample(0, Some(&quota([100.0, 20.0], now)));
        let spent = read_sample(&fixture.state, &fixture.accounts[0].home).unwrap();
        assert!(!needs_refresh(
            Some(&spent),
            spent.attempted_at + FAILURE_RETRY_SECONDS
        ));
        assert!(needs_refresh(
            Some(&spent),
            spent.attempted_at + REFRESH_SECONDS
        ));
        assert_eq!(
            exclusion(Some(&spent), spent.attempted_at),
            "a limit is used up"
        );
        let mut reset = quota([10.0, 20.0], now);
        reset.windows[0].resets_at = Some(now);
        fixture.sample(1, Some(&reset));
        let reset = read_sample(&fixture.state, &fixture.accounts[1].home).unwrap();
        assert!(needs_refresh(
            Some(&reset),
            reset.attempted_at + FAILURE_RETRY_SECONDS
        ));
        assert_eq!(
            exclusion(Some(&reset), reset.attempted_at),
            "a limit reset since its report"
        );
        fixture.sample(1, None);
        let failed = read_sample(&fixture.state, &fixture.accounts[1].home);
        assert_eq!(exclusion(failed.as_ref(), now), "quota read failed");
        assert_eq!(exclusion(None, now), "quota not read in time");
        let message = no_account(&[
            "a (a limit is used up)".into(),
            "b (quota read failed)".into(),
        ]);
        assert!(message.contains(": a (a limit is used up), b (quota read failed)."));
        assert!(no_account(&[]).contains("system default is not balanced"));
    }

    #[test]
    fn a_read_cut_short_by_the_shared_deadline_keeps_the_usable_sample() {
        let fixture = Fixture::new();
        let now = unix_now();
        // Due for a refresh, yet still fresh enough to choose.
        let sample = Sample {
            home: fixture.accounts[0].home.clone(),
            attempted_at: now - REFRESH_SECONDS - 5,
            usage: Some(quota([10.0, 20.0], now - REFRESH_SECONDS - 5)),
        };
        let directory = cache_directory(&fixture.state).unwrap();
        fs::write(
            sample_path(&directory, &fixture.accounts[0].home),
            serde_json::to_vec(&sample).unwrap(),
        )
        .unwrap();
        refresh_samples_with(
            &fixture.state,
            &fixture.accounts[..1],
            Duration::from_millis(80),
            |_, timeout| {
                thread::sleep(timeout);
                Err("fixture timeout".into())
            },
        )
        .unwrap();
        let kept = read_sample(&fixture.state, &fixture.accounts[0].home).unwrap();
        assert_eq!(kept.attempted_at, sample.attempted_at);
        assert!(headroom(kept.usage.as_ref().unwrap(), unix_now()).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn cache_is_private_and_rejects_symlinks_oversized_samples_and_wrong_homes() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let fixture = Fixture::new();
        fixture.sample(0, Some(&quota([1.0, 2.0], unix_now())));
        let directory = fixture.state.join(CACHE_DIRECTORY);
        let path = sample_path(&directory, &fixture.accounts[0].home);
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut sample = read_sample(&fixture.state, &fixture.accounts[0].home).unwrap();
        sample.home = fixture.accounts[1].home.clone();
        fs::write(&path, serde_json::to_vec(&sample).unwrap()).unwrap();
        assert!(read_sample(&fixture.state, &fixture.accounts[0].home).is_none());
        fs::write(&path, vec![b' '; MAX_SAMPLE_BYTES as usize + 1]).unwrap();
        assert!(read_sample(&fixture.state, &fixture.accounts[0].home).is_none());
        fs::remove_file(&path).unwrap();
        let outside = fixture.root.join("outside");
        fs::write(&outside, serde_json::to_vec(&sample).unwrap()).unwrap();
        symlink(&outside, &path).unwrap();
        assert!(read_sample(&fixture.state, &fixture.accounts[0].home).is_none());
        fixture.sample(0, None);
        assert!(!fs::symlink_metadata(path).unwrap().file_type().is_symlink());
        assert_eq!(
            fs::read(&outside).unwrap(),
            serde_json::to_vec(&sample).unwrap()
        );
    }

    // Real account discovery is intentionally never run by tests. A subprocess
    // supplies only a disposable Orca metadata profile, with no credentials.
    #[test]
    fn new_chats_balance_but_explicit_accounts_project_pins_and_disabled_policy_stay_fixed() {
        const CHILD: &str = "RIWORK_BALANCING_POLICY_FIXTURE";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = PathBuf::from(root);
            let state = root.join("state");
            let store = SettingsStore::open(&state).unwrap();
            store
                .update(|settings| {
                    settings.selected_codex_account = Some("a".into());
                    settings.codex_subscription_balancing = true;
                })
                .unwrap();
            let project_store = Store::open(&state).unwrap();
            let project_root = root.join("project");
            fs::create_dir_all(&project_root).unwrap();
            let project = project_store
                .add_project(&project_root, Some("Fixture"))
                .unwrap();
            let a = codex_accounts::resolve_launch_binding(&state, Some("a")).unwrap();
            let b = codex_accounts::resolve_launch_binding(&state, Some("b")).unwrap();
            record_result(&state, &a.home, Some(&quota([5.0, 85.0], unix_now()))).unwrap();
            record_result(&state, &b.home, Some(&quota([40.0, 40.0], unix_now()))).unwrap();
            assert_eq!(new_binding(&state, None).unwrap(), b);
            assert_eq!(new_binding(&state, Some(&project.id)).unwrap(), b);
            // A catalog's account is then only a preview the New Chat dialog must not pin.
            assert!(balances(&state, None) && balances(&state, Some(&project.id)));
            assert!(
                crate::chat::catalog::saved(
                    &state,
                    Some(&project.id),
                    crate::chat::model::Provider::Codex
                )
                .balanced
            );
            assert_eq!(
                crate::chat::launch::account_for(
                    &state,
                    crate::chat::model::Provider::Codex,
                    Some(&project.id),
                    None
                )
                .unwrap()
                .as_deref(),
                Some("b")
            );
            assert_eq!(
                crate::chat::launch::account_for(
                    &state,
                    crate::chat::model::Provider::Codex,
                    Some(&project.id),
                    Some("a")
                )
                .unwrap()
                .as_deref(),
                Some("a")
            );
            project_store
                .set_project_codex_account(&project.id, ProjectCodexAccount::Saved("a".into()))
                .unwrap();
            assert_eq!(new_binding(&state, Some(&project.id)).unwrap(), a);
            assert!(!balances(&state, Some(&project.id)));
            project_store
                .set_project_codex_account(&project.id, ProjectCodexAccount::SystemDefault)
                .unwrap();
            assert_eq!(new_binding(&state, Some(&project.id)).unwrap().id, None);
            assert!(!balances(&state, Some(&project.id)));
            store
                .update(|settings| settings.codex_subscription_balancing = false)
                .unwrap();
            assert_eq!(new_binding(&state, None).unwrap(), a);
            assert!(!balances(&state, None));
            // Enabling is not an account mutation: the configured preference stays.
            assert_eq!(
                store.load().unwrap().selected_codex_account.as_deref(),
                Some("a")
            );
            store
                .update(|settings| settings.codex_subscription_balancing = true)
                .unwrap();
            record_result(&state, &a.home, None).unwrap();
            record_result(&state, &b.home, None).unwrap();
            let error = new_binding(&state, None).unwrap_err();
            assert!(
                error.contains("no account with fresh, usable quota"),
                "{error}"
            );
            assert!(error.contains("(quota read failed)"), "{error}");
            project_store
                .set_project_codex_account(&project.id, ProjectCodexAccount::Inherit)
                .unwrap();
            let target = crate::schedules::Target::bind_new_chat(
                &state,
                &project_store.snapshot().unwrap(),
                &project.id,
                crate::chat::model::Provider::Codex,
                None,
                None,
                false,
                Default::default(),
                Some(codex_accounts::SYSTEM_DEFAULT_ID),
            )
            .unwrap();
            target
                .validate_new_chat(&state, &project_store.snapshot().unwrap())
                .expect("saved automations retain the system account even while balancing has no usable account");
            assert_eq!(
                crate::chat::launch::account_for(
                    &state,
                    crate::chat::model::Provider::Codex,
                    None,
                    Some("a")
                )
                .unwrap()
                .as_deref(),
                Some("a")
            );
            return;
        }
        let fixture = Fixture::new();
        let profile = fixture.root.join("orca-profile");
        for id in ["a", "b"] {
            fs::create_dir_all(profile.join("codex-accounts").join(id).join("home")).unwrap();
        }
        let cache = fixture.state.join("codex-accounts.json");
        fs::write(&cache, serde_json::to_vec(&json!({
            "version":1,"user_data":profile,"source_active_id":"a",
            "accounts":[{"id":"a","managedHomeRuntime":"host"},{"id":"b","managedHomeRuntime":"host"}]
        })).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(cache, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "codex_balancing::tests::new_chats_balance_but_explicit_accounts_project_pins_and_disabled_policy_stay_fixed", "--nocapture"])
            .env(CHILD, &fixture.root)
            .env("ORCA_USER_DATA_PATH", &profile)
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
