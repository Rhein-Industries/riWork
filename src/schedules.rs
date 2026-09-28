//! Durable, at-most-once scheduled prompt attempts. No harnesses are created here.
use crate::{
    sessions::{HarnessKind, SessionManager, ShellKind, ShellSession},
    store::{State, Store},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

pub const GRACE_SECONDS: u64 = 300;
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SCHEDULES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum Scope {
    App,
    Project {
        project_id: String,
    },
    Workspace {
        project_id: String,
        worktree_id: String,
    },
}
impl Scope {
    pub fn label(&self) -> &'static str {
        match self {
            Self::App => "APP",
            Self::Project { .. } => "PROJECT",
            Self::Workspace { .. } => "WORKSPACE",
        }
    }
    pub fn visible(&self, project: &str) -> bool {
        match self {
            Self::App => true,
            Self::Project { project_id } | Self::Workspace { project_id, .. } => {
                project_id == project
            }
        }
    }
    pub fn matches(&self, state: &State, shell: &ShellSession) -> bool {
        match self {
            Self::App => {
                shell.kind == ShellKind::Orchestrator
                    && shell.project_id.is_none()
                    && shell.worktree_id.is_none()
            }
            Self::Project { project_id } => {
                state.projects.iter().any(|p| &p.id == project_id)
                    && shell.kind == ShellKind::Orchestrator
                    && shell.project_id.as_ref() == Some(project_id)
                    && shell.worktree_id.is_none()
            }
            Self::Workspace {
                project_id,
                worktree_id,
            } => {
                state
                    .worktrees
                    .iter()
                    .any(|w| &w.id == worktree_id && &w.project_id == project_id)
                    && shell.kind == ShellKind::Project
                    && shell.project_id.as_ref() == Some(project_id)
                    && shell.worktree_id.as_ref() == Some(worktree_id)
            }
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub scope: Scope,
    pub shell_id: String,
    pub created_at: u64,
    pub command: Option<String>,
    pub harness: HarnessKind,
    pub codex_home: Option<PathBuf>,
    pub pane_identity: String,
    pub provider_session: String,
}
impl Target {
    pub fn bind(
        scope: Scope,
        state: &State,
        sessions: &SessionManager,
        id: &str,
    ) -> Result<Self, String> {
        canonical_id(id)?;
        let shell = sessions.get(id)?;
        if !shell.alive || !scope.matches(state, &shell) {
            return Err("Select an existing live session in this scope".into());
        }
        let harness = shell
            .harness
            .ok_or("Scheduling requires an existing Codex or Claude session")?;
        if harness == HarnessKind::Grok {
            return Err("Grok scheduling is not yet supported".into());
        }
        Ok(Self {
            scope,
            shell_id: id.into(),
            created_at: shell.created_at_unix,
            command: shell.command.clone(),
            harness,
            codex_home: shell.codex_home.clone(),
            pane_identity: sessions.schedule_pane_identity(id)?,
            provider_session: sessions.schedule_provider_identity(&shell)?,
        })
    }
    pub fn matches(&self, state: &State, shell: &ShellSession) -> bool {
        self.shell_id == shell.id
            && self.created_at == shell.created_at_unix
            && self.command == shell.command
            && Some(self.harness) == shell.harness
            && self.codex_home == shell.codex_home
            && self.scope.matches(state, shell)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Timing {
    Once { at: u64 },
    Interval { first: u64, seconds: u64 },
}
impl Timing {
    pub fn first(&self) -> u64 {
        match self {
            Self::Once { at } => *at,
            Self::Interval { first, .. } => *first,
        }
    }
    pub fn next_after(&self, now: u64) -> Option<u64> {
        match self {
            Self::Once { .. } => None,
            Self::Interval { first, seconds } => {
                if now < *first {
                    return Some(*first);
                }
                let count = now
                    .saturating_sub(*first)
                    .checked_div(*seconds)?
                    .checked_add(1)?;
                first.checked_add(count.checked_mul(*seconds)?)
            }
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.first() > 253402300799 {
            return Err("Date must be before year 10000".into());
        }
        if let Self::Interval { seconds, .. } = self
            && (!(300..=31_536_000).contains(seconds) || seconds % 60 != 0)
        {
            return Err("Repeat interval must be whole minutes, from 5 minutes to 365 days".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Dispatching,
    Submitted,
    Deferred,
    Missed,
    Failed,
    Uncertain,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub due_at: u64,
    pub observed_at: u64,
    pub outcome: Outcome,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub revision: u64,
    pub title: String,
    pub prompt: String,
    pub target: Target,
    pub timing: Timing,
    #[serde(default)]
    pub review_required: bool,
    pub paused: bool,
    pub next_run: Option<u64>,
    pub last_run: Option<Run>,
    pub check_after: u64,
}
#[derive(Clone, Debug)]
pub enum Delivery {
    Submitted,
    Deferred(String),
    Failed(String),
    Uncertain(String),
}
#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    window_started: u64,
    #[serde(default)]
    sends_in_window: u8,
    #[serde(default)]
    schedules: Vec<Schedule>,
    #[serde(default)]
    consumed: Vec<(String, String)>,
}
#[derive(Clone)]
pub struct ScheduleStore {
    home: PathBuf,
}
impl ScheduleStore {
    pub fn at(home: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&home).map_err(|e| e.to_string())?;
        Ok(Self { home })
    }
    fn lock(&self) -> Result<File, String> {
        private_file(&self.home.join("schedules.lock"), false)
    }
    fn read(&self) -> Result<Ledger, String> {
        let path = self.home.join("schedules.json");
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ledger::default()),
            Err(e) => return Err(format!("Cannot read schedules: {e}")),
        };
        if file.metadata().map_err(|e| e.to_string())?.len() > MAX_BYTES {
            return Err("Schedule store exceeds 8 MiB".into());
        }
        let ledger: Ledger = serde_json::from_reader(file.take(MAX_BYTES + 1))
            .map_err(|e| format!("Invalid schedule store: {e}"))?;
        if ledger.schedules.len() > MAX_SCHEDULES {
            return Err("Too many schedules".into());
        }
        for schedule in &ledger.schedules {
            canonical_id(&schedule.id)?;
            canonical_id(&schedule.target.shell_id)?;
            schedule.timing.validate()?;
        }
        Ok(ledger)
    }
    fn write(&self, ledger: &Ledger) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(ledger).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("Schedule store exceeds 8 MiB".into());
        }
        let tmp = self.home.join(format!(".schedules-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = private_file(&tmp, true)?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| format!("Cannot sync schedules: {e}"))?;
            fs::rename(&tmp, self.home.join("schedules.json")).map_err(|e| e.to_string())?;
            File::open(&self.home)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
    pub fn list(&self) -> Result<Vec<Schedule>, String> {
        let lock = self.lock()?;
        FileExt::lock_shared(&lock).map_err(|e| e.to_string())?;
        Ok(self.read()?.schedules)
    }
    fn mutate<T>(&self, f: impl FnOnce(&mut Ledger) -> Result<T, String>) -> Result<T, String> {
        let lock = self.lock()?;
        FileExt::lock_exclusive(&lock).map_err(|e| e.to_string())?;
        let mut ledger = self.read()?;
        let result = f(&mut ledger)?;
        self.write(&ledger)?;
        Ok(result)
    }
    pub fn save(
        &self,
        previous: Option<(&str, u64)>,
        title: String,
        prompt: String,
        target: Target,
        timing: Timing,
        now: u64,
    ) -> Result<Schedule, String> {
        timing.validate()?;
        canonical_id(&target.shell_id)?;
        if timing.first() <= now {
            return Err("First run must be in the future".into());
        }
        if title.trim().is_empty() || title.len() > 120 {
            return Err("Enter a title (up to 120 bytes)".into());
        }
        if prompt.trim().is_empty()
            || prompt.len() > 16 * 1024
            || prompt.chars().any(|c| c.is_control())
        {
            return Err("Enter a single-line prompt (up to 16 KiB, no control characters)".into());
        }
        self.mutate(|ledger| {
            let (id, revision, paused, last_run) = if let Some((id, revision)) = previous {
                let existing = ledger
                    .schedules
                    .iter()
                    .find(|s| s.id == id)
                    .ok_or("Schedule was deleted")?;
                if existing.revision != revision {
                    return Err("Schedule or run status changed; reopen it before saving".into());
                }
                (
                    id.to_owned(),
                    revision + 1,
                    existing.paused && !existing.review_required,
                    existing.last_run.clone(),
                )
            } else {
                if ledger.schedules.len() >= MAX_SCHEDULES {
                    return Err("Schedule limit reached (512)".into());
                }
                (Uuid::new_v4().to_string(), 1, false, None)
            };
            let schedule = Schedule {
                id: id.clone(),
                revision,
                title: title.trim().into(),
                prompt,
                target,
                next_run: Some(timing.first()),
                timing,
                paused,
                review_required: false,
                last_run,
                check_after: 0,
            };
            ledger.schedules.retain(|s| s.id != id);
            ledger.schedules.push(schedule.clone());
            Ok(schedule)
        })
    }
    pub fn pause(
        &self,
        id: &str,
        revision: u64,
        paused: bool,
        now: u64,
    ) -> Result<Schedule, String> {
        self.mutate(|l| {
            let s = l
                .schedules
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or("Schedule was deleted")?;
            if s.revision != revision {
                return Err("Schedule changed; refresh and try again".into());
            }
            if !paused && s.review_required {
                return Err("Edit and save a future run after reviewing this outcome".into());
            }
            if !paused && s.next_run.is_none() {
                return Err("Edit the schedule to choose a new future run".into());
            }
            if !paused && s.next_run.is_some_and(|n| n <= now) {
                s.next_run = s.timing.next_after(now);
                if s.next_run.is_none() {
                    return Err("Edit the one-time schedule to choose a future run".into());
                }
            }
            s.paused = paused;
            s.check_after = 0;
            s.revision += 1;
            Ok(s.clone())
        })
    }
    pub fn delete(&self, id: &str, revision: u64) -> Result<(), String> {
        self.mutate(|l| {
            let s = l
                .schedules
                .iter()
                .find(|s| s.id == id)
                .ok_or("Schedule was deleted")?;
            if s.revision != revision {
                return Err("Schedule changed; refresh and try again".into());
            }
            l.schedules.retain(|s| s.id != id);
            Ok(())
        })
    }
    #[cfg(test)]
    pub fn tick(&self, now: u64) -> Result<(), String> {
        self.tick_tracked(now, &mut std::collections::HashMap::new())
    }
    fn tick_tracked(
        &self,
        now: u64,
        trackers: &mut std::collections::HashMap<String, crate::activity::ActivityTracker>,
    ) -> Result<(), String> {
        let ids: std::collections::HashSet<_> = self
            .list()?
            .into_iter()
            .map(|s| s.target.shell_id)
            .collect();
        trackers.retain(|id, _| ids.contains(id));
        self.tick_with(now, |target, prompt, claim| {
            let sessions = SessionManager::at(self.home.clone())?;
            let state = Store::open(self.home.clone())?.snapshot()?;
            let tracker = trackers
                .entry(target.shell_id.clone())
                .or_insert_with(|| crate::activity::ActivityTracker::at(self.home.clone()));
            sessions.send_scheduled(target, &state, prompt, tracker, claim)
        })
    }
    // Hold the same cross-process lock for CRUD and dispatch. A durable claim is
    // synced before any terminal mutation. A dead owner leaves an uncertain run.
    fn tick_with(
        &self,
        now: u64,
        mut dispatch: impl FnMut(
            &Target,
            &str,
            &mut dyn FnMut(&str) -> Result<bool, String>,
        ) -> Result<Delivery, String>,
    ) -> Result<(), String> {
        let lock = self.lock()?;
        match FileExt::try_lock_exclusive(&lock) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) => return Err(e.to_string()),
        }
        let mut ledger = self.read()?;
        let mut changed = false;
        for s in &mut ledger.schedules {
            if let Some(run) = &mut s.last_run
                && run.outcome == Outcome::Dispatching
            {
                run.outcome = Outcome::Uncertain;
                run.message = "Delivery was interrupted; review the target before scheduling again. No automatic retry.".into();
                s.paused = true;
                s.review_required = true;
                s.revision += 1;
                changed = true;
            }
        }
        if changed {
            self.write(&ledger)?;
        }
        // One attempt per tick; oldest due first. Never replay a backlog.
        let index = ledger
            .schedules
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                !s.paused && s.next_run.is_some_and(|n| n <= now) && s.check_after <= now
            })
            .min_by_key(|(_, s)| s.next_run)
            .map(|(i, _)| i);
        let Some(index) = index else {
            return Ok(());
        };
        let s = ledger.schedules[index].clone();
        let due = s.next_run.unwrap();
        if now.saturating_sub(due) > GRACE_SECONDS {
            let current = &mut ledger.schedules[index];
            current.next_run = current.timing.next_after(now);
            current.last_run = Some(Run {
                due_at: due,
                observed_at: now,
                outcome: Outcome::Missed,
                message: "Missed the 5-minute delivery window; skipped overdue occurrences.".into(),
            });
            current.revision += 1;
            return self.write(&ledger);
        }
        let mut claimed = false;
        let result = dispatch(&s.target, &s.prompt, &mut |token| {
            if now >= ledger.window_started.saturating_add(60) {
                ledger.window_started = now;
                ledger.sends_in_window = 0;
            }
            if ledger.sends_in_window >= 4 {
                return Ok(false);
            }
            if ledger
                .consumed
                .iter()
                .any(|(id, t)| id == &s.target.shell_id && t == token)
            {
                return Ok(false);
            }
            ledger.sends_in_window += 1;
            ledger.consumed.retain(|(id, _)| id != &s.target.shell_id);
            ledger
                .consumed
                .push((s.target.shell_id.clone(), token.into()));
            // Keep only identities still referenced by schedules.
            ledger
                .consumed
                .retain(|(id, _)| ledger.schedules.iter().any(|s| &s.target.shell_id == id));
            let current = &mut ledger.schedules[index];
            current.next_run = current.timing.next_after(now);
            current.last_run = Some(Run {
                due_at: due,
                observed_at: now,
                outcome: Outcome::Dispatching,
                message: "Delivery in progress".into(),
            });
            current.revision += 1;
            self.write(&ledger)?;
            claimed = true;
            Ok(true)
        })?;
        let (outcome, message, pause) = match result {
            Delivery::Submitted => (
                Outcome::Submitted,
                "Submitted once to the pinned session; agent completion is separate.".into(),
                false,
            ),
            Delivery::Deferred(e) => (Outcome::Deferred, e, false),
            Delivery::Failed(e) => (Outcome::Failed, e, true),
            Delivery::Uncertain(e) => (Outcome::Uncertain, e, true),
        };
        if !claimed && matches!(outcome, Outcome::Submitted | Outcome::Uncertain) {
            return Err("Delivery result lacked a durable claim".into());
        }
        if claimed && outcome == Outcome::Deferred {
            return Err("Claimed delivery cannot be deferred".into());
        }
        let current = &mut ledger.schedules[index];
        if outcome != Outcome::Deferred {
            current.next_run = current.timing.next_after(now);
        } else {
            current.check_after = now.saturating_add(15);
        }
        current.last_run = Some(Run {
            due_at: due,
            observed_at: now,
            outcome,
            message,
        });
        current.paused |= pause;
        current.review_required |= pause;
        current.revision += 1;
        self.write(&ledger)
    }
}
fn private_file(path: &Path, new: bool) -> Result<File, String> {
    if fs::symlink_metadata(path).is_ok_and(|m| !m.is_file()) {
        return Err("Schedule state must be a regular file".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if new {
        options.create_new(true);
    } else {
        options.create(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|e| format!("Cannot open schedule state: {e}"))
}
fn canonical_id(id: &str) -> Result<(), String> {
    if Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
        Ok(())
    } else {
        Err("Scheduling requires full canonical UUIDs".into())
    }
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Default)]
pub struct RuntimeStatus(pub Option<String>);
impl gpui::Global for RuntimeStatus {}
pub fn start(home: PathBuf, cx: &mut gpui::App) {
    cx.set_global(RuntimeStatus::default());
    cx.spawn(async move |cx| {
        let mut trackers = std::collections::HashMap::new();
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let path = home.clone();
            let (returned_trackers, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = ScheduleStore::at(path)
                        .and_then(|store| store.tick_tracked(now(), &mut trackers));
                    (trackers, result)
                })
                .await;
            trackers = returned_trackers;
            cx.update(|cx| {
                let error = result.err();
                if cx.global::<RuntimeStatus>().0 != error {
                    cx.set_global(RuntimeStatus(error));
                }
            });
        }
    })
    .detach();
}
#[cfg(test)]
#[path = "schedules_tests.rs"]
mod tests;
