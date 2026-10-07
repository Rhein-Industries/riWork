//! Project-scoped completion alerts. Only identifiers are queued; no agent text.
//!
//! A durable claim prevents multiple windows/processes from notifying twice.
//! Native delivery stays in the GUI process so notifications belong to RiWork.
//! Only an app-bundle process claims alerts: macOS refuses to post from any
//! other binary, so an unbundled dev build sharing RIWORK_HOME would consume
//! alerts that nothing can show.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use gpui::{App, SystemNotification, SystemNotificationAction};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    sessions::{HarnessKind, SessionManager},
    store::{Project, Store},
};

const LEDGER: &str = "agent-notifications.json";
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const ALERT_LIFETIME: u64 = 5 * 60;
const RECEIPT_LIFETIME: u64 = 7 * 24 * 60 * 60;

#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    entries: Vec<Entry>,
}

#[derive(Default, Deserialize)]
struct RawLedger {
    #[serde(default)]
    entries: Vec<serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    #[serde(default = "completion_id")]
    completion_id: String,
    provider: HarnessKind,
    shell_id: String,
    project_id: String,
    event_id: String,
    created_at: u64,
    claimed: bool,
}

#[derive(Debug)]
pub struct CompletionNotice {
    pub completion_id: String,
    pub project_id: String,
    pub shell_id: String,
    pub project_name: String,
    pub worktree: Option<String>,
    pub provider: HarnessKind,
}

impl CompletionNotice {
    pub fn notification(&self) -> SystemNotification {
        let agent = match self.provider {
            HarnessKind::Codex => "Codex",
            HarnessKind::Claude => "Claude",
            HarnessKind::Grok => "Grok",
        };
        let scope = self
            .worktree
            .as_ref()
            .map(|branch| format!(" · {branch}"))
            .unwrap_or_default();
        SystemNotification {
            tag: format!(
                "riwork-agent-done:{}:{}:{}",
                self.project_id, self.shell_id, self.completion_id
            )
            .into(),
            title: format!("{} · {agent} finished", self.project_name).into(),
            body: format!(
                "Agent {} completed its turn{scope}.",
                self.shell_id.chars().take(8).collect::<String>()
            )
            .into(),
            actions: vec![SystemNotificationAction {
                id: "open".into(),
                label: "Open agent".into(),
            }],
        }
    }
}

/// Enqueue a verified completion from a hook or an exact lifecycle binding.
/// Disabled and unscoped sessions do not create a notification ledger.
pub fn record_completion(
    home: &Path,
    shell_id: &str,
    event_id: &str,
    provider: HarnessKind,
) -> Result<bool, String> {
    record_at(home, shell_id, event_id, provider, now())
}

fn record_at(
    home: &Path,
    shell_id: &str,
    event_id: &str,
    provider: HarnessKind,
    time: u64,
) -> Result<bool, String> {
    if !canonical_uuid(shell_id)
        || event_id.is_empty()
        || event_id.len() > 1024
        || event_id.chars().any(char::is_control)
    {
        return Err("Invalid agent completion identity".into());
    }
    let shell = SessionManager::at(home.to_path_buf())?.registered_session(shell_id)?;
    if shell.harness != Some(provider) {
        return Ok(false);
    }
    let Some(project_id) = shell.project_id.as_deref() else {
        return Ok(false);
    };
    if !canonical_uuid(project_id) {
        return Err("Invalid agent project identity".into());
    }
    let state = Store::open(home.to_path_buf())?.snapshot()?;
    let Ok(project) = state.project(project_id) else {
        return Ok(false);
    };
    if !project.notify_on_agent_done {
        return Ok(false);
    }
    let _lock = lock(home)?;
    let mut ledger = read(home)?;
    prune(&mut ledger, time);
    if ledger.entries.iter().any(|entry| {
        entry.provider == provider && entry.shell_id == shell_id && entry.event_id == event_id
    }) {
        return Ok(false);
    }
    ledger.entries.push(Entry {
        completion_id: completion_id(),
        provider,
        shell_id: shell_id.into(),
        project_id: project_id.into(),
        event_id: event_id.into(),
        created_at: time,
        claimed: false,
    });
    if ledger.entries.len() > MAX_ENTRIES {
        ledger.entries.remove(0);
    }
    write(home, &ledger)?;
    Ok(true)
}

/// Atomically claim fresh, still-enabled alerts. Receipts survive GUI reloads.
pub fn claim_pending(home: &Path) -> Result<Vec<CompletionNotice>, String> {
    claim_at(home, now())
}

/// A rapid off/on toggle must cancel work queued before the bell was disabled.
pub fn cancel_pending(home: &Path, project_id: &str) -> Result<(), String> {
    if !home.join(LEDGER).exists() {
        return Ok(());
    }
    let _lock = lock(home)?;
    let mut ledger = read(home)?;
    let mut changed = false;
    for entry in &mut ledger.entries {
        if entry.project_id == project_id && !entry.claimed {
            entry.claimed = true;
            changed = true;
        }
    }
    if changed {
        write(home, &ledger)?;
    }
    Ok(())
}

fn claim_at(home: &Path, time: u64) -> Result<Vec<CompletionNotice>, String> {
    if !home.join(LEDGER).exists() {
        return Ok(Vec::new());
    }
    let _lock = lock(home)?;
    let mut ledger = read(home)?;
    let before = ledger.entries.len();
    prune(&mut ledger, time);
    if !ledger.entries.iter().any(|entry| !entry.claimed) {
        if before != ledger.entries.len() {
            write(home, &ledger)?;
        }
        return Ok(Vec::new());
    }
    let state = Store::open(home.to_path_buf())?.snapshot()?;
    let manager = SessionManager::at(home.to_path_buf())?;
    let mut notices = Vec::new();
    for entry in ledger.entries.iter_mut().filter(|entry| !entry.claimed) {
        // Claim skipped events too: re-enabling a bell must not replay old work.
        entry.claimed = true;
        if time.saturating_sub(entry.created_at) > ALERT_LIFETIME {
            continue;
        }
        let Ok(project) = state.project(&entry.project_id) else {
            continue;
        };
        if !project.notify_on_agent_done {
            continue;
        }
        let Ok(shell) = manager.registered_session(&entry.shell_id) else {
            continue;
        };
        if shell.project_id.as_deref() != Some(&entry.project_id)
            || shell.harness != Some(entry.provider)
        {
            continue;
        }
        let worktree = shell
            .worktree_id
            .as_ref()
            .and_then(|id| {
                state
                    .worktrees
                    .iter()
                    .find(|worktree| &worktree.id == id && worktree.project_id == project.id)
            })
            .map(|worktree| compact_label(&worktree.branch));
        notices.push(CompletionNotice {
            completion_id: entry.completion_id.clone(),
            project_id: project.id.clone(),
            shell_id: shell.id,
            project_name: compact_label(&project.name),
            worktree,
            provider: entry.provider,
        });
    }
    write(home, &ledger)?;
    Ok(notices)
}

pub fn show_enabled(project: &Project, cx: &App) {
    // On macOS this first user-initiated post asks for notification permission.
    // Subsequent completions use the same RiWork app-bundle notification center.
    cx.show_system_notification(SystemNotification {
        tag: format!("riwork-notifications-enabled:{}", project.id).into(),
        title: compact_label(&project.name).into(),
        body: "Agent completion notifications enabled.".into(),
        actions: Vec::new(),
    });
}

pub fn response_target(tag: &str) -> Option<(&str, &str)> {
    let mut parts = tag.strip_prefix("riwork-agent-done:")?.split(':');
    let (project_id, shell_id, completion_id) = (parts.next()?, parts.next()?, parts.next()?);
    (parts.next().is_none()
        && canonical_uuid(project_id)
        && canonical_uuid(shell_id)
        && canonical_uuid(completion_id))
    .then_some((project_id, shell_id))
}

/// True for `X.app/Contents/MacOS/<binary>`, the layout that gives macOS (and
/// gpui's notification center) a bundle identifier to post under.
fn in_app_bundle(executable: &Path) -> bool {
    let mut parents = executable.ancestors().skip(1);
    parents
        .next()
        .is_some_and(|dir| dir.ends_with("Contents/MacOS"))
        && parents
            .nth(1)
            .is_some_and(|app| app.extension().is_some_and(|ext| ext == "app"))
}

pub fn start(home: PathBuf, cx: &mut App) {
    if !std::env::current_exe().is_ok_and(|executable| in_app_bundle(&executable)) {
        return;
    }
    cx.spawn(async move |cx| {
        let mut last_error = None;
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let task_home = home.clone();
            let work = cx
                .background_executor()
                .spawn(async move { claim_pending(&task_home) });
            match work.await {
                Ok(notices) => {
                    last_error = None;
                    cx.update(|cx| {
                        for notice in notices {
                            cx.show_system_notification(notice.notification());
                        }
                    });
                }
                Err(error) => {
                    if last_error.as_ref() != Some(&error) {
                        eprintln!("riwork notifications: {error}");
                        last_error = Some(error);
                    }
                }
            }
        }
    })
    .detach();
}

fn compact_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(120)
        .collect()
}

fn canonical_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn completion_id() -> String {
    Uuid::new_v4().to_string()
}

fn prune(ledger: &mut Ledger, time: u64) {
    ledger
        .entries
        .retain(|entry| time.saturating_sub(entry.created_at) <= RECEIPT_LIFETIME);
}

fn lock(home: &Path) -> Result<File, String> {
    let path = home.join("agent-notifications.lock");
    reject_symlink(&path)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|_| "Cannot open the notification lock")?;
    file.lock_exclusive()
        .map_err(|_| "Cannot lock agent notifications")?;
    Ok(file)
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            Err("Notification metadata must be a regular file".into())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("Cannot inspect notification metadata".into()),
    }
}

fn read(home: &Path) -> Result<Ledger, String> {
    let path = home.join(LEDGER);
    reject_symlink(&path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Ledger::default()),
        Err(_) => return Err("Cannot read agent notifications".into()),
    };
    if file
        .metadata()
        .map_err(|_| "Cannot inspect agent notifications")?
        .len()
        > MAX_BYTES
    {
        return Err("Agent notification metadata is too large".into());
    }
    // Entries are checked one by one: a single damaged or foreign entry (an
    // older or newer build's format) must not disable every alert. Skipped
    // entries are dropped the next time the ledger is written.
    let raw: RawLedger = serde_json::from_reader(file.take(MAX_BYTES))
        .map_err(|_| "Invalid agent notification metadata")?;
    let mut entries: Vec<Entry> = raw
        .entries
        .into_iter()
        .filter_map(|value| serde_json::from_value::<Entry>(value).ok())
        .filter(valid_entry)
        .collect();
    if entries.len() > MAX_ENTRIES {
        entries.drain(..entries.len() - MAX_ENTRIES);
    }
    Ok(Ledger { entries })
}

fn valid_entry(entry: &Entry) -> bool {
    canonical_uuid(&entry.completion_id)
        && canonical_uuid(&entry.shell_id)
        && canonical_uuid(&entry.project_id)
        && !entry.event_id.is_empty()
        && entry.event_id.len() <= 1024
        && !entry.event_id.chars().any(char::is_control)
}

fn write(home: &Path, ledger: &Ledger) -> Result<(), String> {
    let path = home.join(LEDGER);
    reject_symlink(&path)?;
    let temporary = home.join(format!(".agent-notifications-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| "Cannot stage agent notifications")?;
        serde_json::to_writer(&mut file, ledger)
            .map_err(|_| "Cannot encode agent notifications")?;
        file.write_all(b"\n")
            .and_then(|_| file.sync_all())
            .map_err(|_| "Cannot save agent notifications")?;
        fs::rename(&temporary, path).map_err(|_| "Cannot install agent notifications")
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{ShellKind, ShellSession};
    use std::sync::{Arc, Barrier};

    struct Fixture {
        home: PathBuf,
        project_id: String,
        shell_id: String,
    }
    impl Fixture {
        fn new() -> Self {
            let home =
                std::env::temp_dir().join(format!("riwork-notifications-{}", Uuid::new_v4()));
            fs::create_dir_all(home.join("project")).unwrap();
            let store = Store::open(home.clone()).unwrap();
            let project = store
                .add_project(home.join("project"), Some("Project A"))
                .unwrap();
            let shell_id = Uuid::new_v4().to_string();
            let shell = ShellSession {
            user_opened: true,
            parent_id: None,

                id: shell_id.clone(),
                project_id: Some(project.id.clone()),
                worktree_id: None,
                kind: ShellKind::Project,
                cwd: project.root.clone(),
                command: None,
                editor_path: None,
                harness: Some(HarnessKind::Codex),
                unrestricted: false,
                codex_account_id: None,
                codex_account_label: None,
                codex_account_email: None,
                codex_home: None,
                orchestrator_skill_loaded: false,
                orchestrator_skill_version: None,
                orchestrator_project_root: None,
                created_at_unix: 1,
                alive: true,
            };
            fs::write(
                home.join("sessions.json"),
                serde_json::json!({"sessions":[shell]}).to_string(),
            )
            .unwrap();
            Self {
                home,
                project_id: project.id,
                shell_id,
            }
        }
        fn enabled(&self, enabled: bool) {
            Store::open(self.home.clone())
                .unwrap()
                .set_project_notifications(&self.project_id, enabled)
                .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn alerts_are_opt_in_and_disabled_pending_work_never_replays() {
        let fixture = Fixture::new();
        assert!(
            !record_at(
                &fixture.home,
                &fixture.shell_id,
                "thread:turn-a",
                HarnessKind::Codex,
                100
            )
            .unwrap()
        );
        assert!(!fixture.home.join(LEDGER).exists());
        fixture.enabled(true);
        assert!(
            record_at(
                &fixture.home,
                &fixture.shell_id,
                "thread:turn-b",
                HarnessKind::Codex,
                100
            )
            .unwrap()
        );
        fixture.enabled(false);
        cancel_pending(&fixture.home, &fixture.project_id).unwrap();
        fixture.enabled(true);
        assert!(claim_at(&fixture.home, 102).unwrap().is_empty());
        assert!(
            record_at(
                &fixture.home,
                &fixture.shell_id,
                "thread:turn-c",
                HarnessKind::Codex,
                103
            )
            .unwrap()
        );
        let notices = claim_at(&fixture.home, 104).unwrap();
        assert_eq!(notices.len(), 1);
        let alert = notices[0].notification();
        assert_eq!(alert.title.as_ref(), "Project A · Codex finished");
        assert_eq!(
            response_target(&alert.tag),
            Some((fixture.project_id.as_str(), fixture.shell_id.as_str()))
        );
        assert_eq!(alert.actions[0].id.as_ref(), "open");
    }

    #[test]
    fn concurrent_hooks_and_windows_claim_each_completion_only_once() {
        let fixture = Fixture::new();
        fixture.enabled(true);
        let barrier = Arc::new(Barrier::new(5));
        let mut workers = Vec::new();
        for _ in 0..4 {
            let (home, shell_id, barrier) = (
                fixture.home.clone(),
                fixture.shell_id.clone(),
                barrier.clone(),
            );
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                record_at(
                    &home,
                    &shell_id,
                    "same-thread:same-turn",
                    HarnessKind::Codex,
                    100,
                )
                .unwrap()
            }));
        }
        barrier.wait();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| usize::from(worker.join().unwrap()))
                .sum::<usize>(),
            1
        );
        let a = fixture.home.clone();
        let b = fixture.home.clone();
        let first = std::thread::spawn(move || claim_at(&a, 101).unwrap().len());
        let second = std::thread::spawn(move || claim_at(&b, 101).unwrap().len());
        assert_eq!(first.join().unwrap() + second.join().unwrap(), 1);
        assert!(
            !record_at(
                &fixture.home,
                &fixture.shell_id,
                "same-thread:same-turn",
                HarnessKind::Codex,
                102
            )
            .unwrap()
        );
        assert!(claim_at(&fixture.home, 102).unwrap().is_empty());
        assert!(
            record_at(
                &fixture.home,
                &fixture.shell_id,
                "same-thread:next-turn",
                HarnessKind::Codex,
                103
            )
            .unwrap()
        );
        assert_eq!(claim_at(&fixture.home, 104).unwrap().len(), 1);
    }

    #[test]
    fn successive_turns_keep_distinct_native_notifications_and_route_to_the_same_agent() {
        let fixture = Fixture::new();
        fixture.enabled(true);
        for event in ["thread:turn-a", "thread:turn-b"] {
            record_at(
                &fixture.home,
                &fixture.shell_id,
                event,
                HarnessKind::Codex,
                100,
            )
            .unwrap();
        }
        let notices = claim_at(&fixture.home, 101).unwrap();
        assert_eq!(notices.len(), 2);
        let a = notices[0].notification();
        let b = notices[1].notification();
        assert_ne!(a.tag, b.tag);
        assert_eq!(response_target(&a.tag), response_target(&b.tag));
        assert_eq!(
            response_target(&a.tag),
            Some((fixture.project_id.as_str(), fixture.shell_id.as_str()))
        );
    }

    #[test]
    fn stale_and_wrong_provider_events_cannot_send_alerts() {
        let fixture = Fixture::new();
        fixture.enabled(true);
        assert!(
            !record_at(
                &fixture.home,
                &fixture.shell_id,
                "claude-turn",
                HarnessKind::Claude,
                100
            )
            .unwrap()
        );
        assert!(
            record_at(
                &fixture.home,
                &fixture.shell_id,
                "old-turn",
                HarnessKind::Codex,
                100
            )
            .unwrap()
        );
        assert!(claim_at(&fixture.home, 401).unwrap().is_empty());
        assert!(response_target("riwork-agent-done:not-a-project:not-a-shell").is_none());
        assert!(response_target("riwork-notifications-enabled:project").is_none());
        assert!(
            record_at(
                &fixture.home,
                &fixture.shell_id,
                "body\ntext",
                HarnessKind::Codex,
                402
            )
            .is_err()
        );
    }

    #[test]
    fn one_invalid_ledger_entry_does_not_disable_the_others() {
        let fixture = Fixture::new();
        fixture.enabled(true);
        let entry = |shell: &str, provider: &str, event: &str| {
            serde_json::json!({
                "completion_id": Uuid::new_v4().to_string(), "provider": provider,
                "shell_id": shell, "project_id": fixture.project_id,
                "event_id": event, "created_at": 100, "claimed": false
            })
        };
        let good = entry(&fixture.shell_id, "codex", "thread:kept");
        let mut missing_field = entry(&fixture.shell_id, "codex", "thread:missing");
        missing_field.as_object_mut().unwrap().remove("claimed");
        let bad = serde_json::json!({"entries": [
            entry("not-a-uuid", "codex", "thread:bad-shell"),
            missing_field,
            entry(&fixture.shell_id, "an-unknown-harness", "thread:unknown-provider"),
            entry(&fixture.shell_id, "codex", "thread:\ncontrol"),
            entry(&fixture.shell_id, "codex", ""),
            "not even an object",
            good,
        ]});
        fs::write(fixture.home.join(LEDGER), bad.to_string()).unwrap();
        assert_eq!(read(&fixture.home).unwrap().entries.len(), 1);
        assert!(
            record_at(
                &fixture.home,
                &fixture.shell_id,
                "thread:next",
                HarnessKind::Codex,
                101
            )
            .unwrap()
        );
        let notices = claim_at(&fixture.home, 102).unwrap();
        assert_eq!(notices.len(), 2);
        // The damaged entries are gone once the ledger has been rewritten.
        assert_eq!(read(&fixture.home).unwrap().entries.len(), 2);
        // A ledger that is not a ledger at all is still refused.
        fs::write(fixture.home.join(LEDGER), "{ truncated").unwrap();
        assert!(read(&fixture.home).is_err());
    }

    #[test]
    fn only_an_app_bundle_process_claims_alerts() {
        assert!(in_app_bundle(Path::new(
            "/Applications/RiWork.app/Contents/MacOS/riwork"
        )));
        assert!(in_app_bundle(Path::new(
            "/tmp/target/debug/RiWork.app/Contents/MacOS/riwork"
        )));
        assert!(!in_app_bundle(Path::new("/tmp/target/debug/riwork")));
        assert!(!in_app_bundle(Path::new(
            "/tmp/target/debug/RiWork.app/riwork"
        )));
        assert!(!in_app_bundle(Path::new("/tmp/Contents/MacOS/riwork")));
        assert!(!in_app_bundle(Path::new("riwork")));
    }

    #[cfg(unix)]
    #[test]
    fn metadata_is_private_and_symlinks_cannot_redirect_it() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let fixture = Fixture::new();
        fixture.enabled(true);
        record_at(
            &fixture.home,
            &fixture.shell_id,
            "turn",
            HarnessKind::Codex,
            100,
        )
        .unwrap();
        assert_eq!(
            fs::metadata(fixture.home.join(LEDGER))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let outside = fixture.home.join("outside");
        fs::write(&outside, "untouched").unwrap();
        fs::remove_file(fixture.home.join(LEDGER)).unwrap();
        symlink(&outside, fixture.home.join(LEDGER)).unwrap();
        assert!(
            record_at(
                &fixture.home,
                &fixture.shell_id,
                "new-turn",
                HarnessKind::Codex,
                101
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(outside).unwrap(), "untouched");
    }
}
