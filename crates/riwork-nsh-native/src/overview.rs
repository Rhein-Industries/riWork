//! The global dashboard's live read: one process service, whatever the window
//! count, that reads what `riwork dashboard --json` reads and publishes the
//! engine's own `dashboard.get` wire (the phone reads the same shape).
//!
//! Reading starts nothing: no chat host (a running one is asked for its chats,
//! else the saved chats are read), chat, shell, orchestrator or tmux server.
//! The optional Haiku summary is only the saved one (`enrich_saved`): no
//! Keychain read, no network request and no refresh is made from here.
//!
//! Nothing polls while no window shows the dashboard. With a subscriber, a
//! home change (store, sessions, chats, settings, automations, usage) wakes a
//! read at most every `CHANGE_GAP`; automations, backends and observed requests
//! are re-read at most every `EXTRAS_EVERY`, which is also the heartbeat that
//! keeps the ages on screen moving (GPUI: 2 s samples, 10 s extras and redraw).
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use nsh_core::{
    JsonValue,
    element::{JsHost, JsIn, JsOwned, JsValue, PropError, TsTypes},
};
use riwork_core::store::{State, Store};
use riwork_engine::{
    activity::{self, AgentState},
    chat::model::ChatInfo,
    global_overview::{
        BackendSample, Backends, Inputs, Tone, backend_lines, build, observed, read_backends,
        read_extras, timed_chats, wire_with_observations,
    },
    orchestrators::{ChatHost, running_chats},
    schedules::Schedule,
    sessions::{SessionManager, ShellSession},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hub::{Hub, TopicSubscription};

/// A change is read at most this often.
pub const CHANGE_GAP: Duration = Duration::from_secs(2);
/// Automations, backends and observed requests; also the age heartbeat.
pub const EXTRAS_EVERY: Duration = Duration::from_secs(10);

/// What the dashboard shows: the last good read, and why a newer one failed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardDto {
    pub schema: u32,
    /// The service clock (Unix seconds) when this was published. Every age on
    /// screen is measured against it, so TSX keeps no clock of its own.
    pub now: u64,
    /// When the shown wire was read; `None` before the first good read.
    pub read_at: Option<u64>,
    /// Why the latest read failed, when it did.
    pub error: Option<String>,
    /// `riwork dashboard --json` (wire v1 with observations and the saved
    /// summary under `intelligence`); `None` until a read succeeded.
    pub wire: Option<Value>,
    /// The Backends section's rows as the engine words them; `None` until read.
    pub backend_lines: Option<Vec<BackendLineDto>>,
}

/// One Backends row (`global_overview::backend_lines`). Rows are not clickable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendLineDto {
    /// "app", "chat-host", or a provider's program name.
    pub key: String,
    pub title: String,
    pub detail: String,
    /// `current`, `stale` (restart to apply), `unknown` or `idle` (not running).
    pub tone: String,
}

/// What one read gives the window.
pub struct Read {
    pub wire: Value,
    pub backend_lines: Option<Vec<BackendLineDto>>,
}

impl JsValue for crate::hub::Snapshot<DashboardDto> {
    fn ts_type(types: &mut TsTypes) -> String {
        types.declare("DashboardEvent", |types| {
            types.declare("DashboardDto", |_| {
                "export interface DashboardDto { schema: number; now: number; readAt: number | null; error: string | null; wire: unknown | null; backendLines: { key: string; title: string; detail: string; tone: 'current' | 'stale' | 'unknown' | 'idle' }[] | null; }".into()
            });
            "export interface DashboardEvent { kind: 'snapshot'; rev: number; data: DashboardDto; resync?: boolean; }".into()
        })
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        serde_json::from_value(JsonValue::from_js(value)?)
            .map_err(|error| PropError(error.to_string()))
    }
    fn to_js<'a>(&self, host: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        serde_json::to_value(self)
            .map_err(|error| PropError(error.to_string()))?
            .to_js(host)
    }
}

/// The shells as one sample of the session registry saw them.
#[derive(Clone, Debug, Default)]
pub struct ShellSample {
    pub shells: Vec<ShellSession>,
    /// Each live shell's current directory.
    pub cwds: BTreeMap<String, PathBuf>,
    /// Each live shell's pane process.
    pub processes: BTreeMap<String, u32>,
}

/// The extras, kept between reads for `EXTRAS_EVERY`.
pub struct Extras {
    schedules: Result<Vec<Schedule>, String>,
    chat_activity: BTreeMap<String, u64>,
    backends: BackendSample,
    observed: observed::Observations,
}

/// One full read. Implementations block; the service calls them off every
/// engine thread.
pub trait OverviewSource: Send + Sync {
    fn home(&self) -> &Path;
    /// The shells, as the session registry says now.
    fn shells(&self) -> Result<ShellSample, String>;
    /// Every chat: a running host's, else the saved ones; `None` when neither
    /// could be read. Never starts a host.
    fn chats(&self) -> Option<Vec<ChatInfo>> {
        let home = self.home();
        let never_start = |_: &Path| -> Result<PathBuf, String> {
            Err("the dashboard starts no chat host".into())
        };
        let host = ChatHost {
            home,
            ensure: &never_start,
        };
        running_chats(&host).or_else(|| riwork_engine::chat::log::read_infos_checked(home).ok())
    }
    fn store(&self) -> Result<State, String> {
        Store::open(self.home().to_path_buf())?.snapshot()
    }
    fn activity(&self, shells: &[ShellSession], now: u64) -> BTreeMap<String, AgentState> {
        activity::states_once(self.home(), shells, now)
    }
    fn extras(
        &self,
        state: &State,
        sample: &ShellSample,
        chats: Option<&[ChatInfo]>,
        now: u64,
    ) -> Extras {
        let home = self.home();
        let (schedules, chat_activity) = read_extras(home, timed_chats(chats));
        Extras {
            schedules,
            chat_activity,
            backends: read_backends(home, &sample.shells, &sample.processes),
            observed: observed::read(home, state, chats, now),
        }
    }
    /// The saved Haiku summary only (DOCS-212): display, never a request.
    fn summary(&self, wire: &mut Value) {
        let _due = riwork_engine::dashboard_intelligence::enrich_saved(self.home(), wire);
    }
}

/// Production: the same calls, in the same order, as `riwork dashboard`.
pub struct EngineSource {
    home: PathBuf,
    manager: OnceLock<SessionManager>,
}

impl EngineSource {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            manager: OnceLock::new(),
        }
    }
}

impl OverviewSource for EngineSource {
    fn home(&self) -> &Path {
        &self.home
    }
    fn shells(&self) -> Result<ShellSample, String> {
        let manager = match self.manager.get() {
            Some(manager) => manager,
            None => {
                let manager = SessionManager::at(self.home.clone())?;
                self.manager.get_or_init(|| manager)
            }
        };
        let sample = manager.sample(false)?;
        Ok(ShellSample {
            shells: sample.shells,
            cwds: sample.directories.unwrap_or_default(),
            processes: sample.processes.unwrap_or_default(),
        })
    }
}

/// Reads `source` once: the wire, with `extras` re-read when due.
pub fn read_once(
    source: &dyn OverviewSource,
    extras: &mut Option<Extras>,
    extras_due: bool,
    now: u64,
) -> Result<Read, String> {
    let state = source.store()?;
    let sample = source.shells()?;
    let activity = source.activity(&sample.shells, now);
    let chats = source.chats();
    if extras_due || extras.is_none() {
        *extras = Some(source.extras(&state, &sample, chats.as_deref(), now));
    }
    let extras = extras.as_ref().expect("extras were just read");
    let overview = build(&Inputs {
        state: &state,
        shells: &sample.shells,
        cwds: &sample.cwds,
        activity: &activity,
        chats: chats.as_deref(),
        chat_activity: &extras.chat_activity,
        schedules: Some(&extras.schedules),
        backends: Some(&extras.backends),
        now,
    });
    let mut wire = wire_with_observations(&overview, now, &extras.observed);
    source.summary(&mut wire);
    let backend_lines = match &overview.backends {
        Backends::NotRead => None,
        Backends::Read(read) => Some(
            backend_lines(read, now)
                .into_iter()
                .map(|line| BackendLineDto {
                    key: line.key.to_owned(),
                    title: line.title,
                    detail: line.detail,
                    tone: match line.tone {
                        Tone::Current => "current",
                        Tone::Stale => "stale",
                        Tone::Unknown => "unknown",
                        Tone::Idle => "idle",
                    }
                    .to_owned(),
                })
                .collect(),
        ),
    };
    Ok(Read {
        wire,
        backend_lines,
    })
}

enum Wake {
    Changed,
    Refresh,
    Subscribed,
}

/// The process-wide reader. Created on the first subscription.
pub struct OverviewService {
    hub: Arc<Hub<DashboardDto>>,
    wake: SyncSender<Wake>,
}

pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// How often the reader may read: production uses `CHANGE_GAP` and `EXTRAS_EVERY`.
#[derive(Clone, Copy, Debug)]
pub struct Pacing {
    pub change_gap: Duration,
    pub extras_every: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            change_gap: CHANGE_GAP,
            extras_every: EXTRAS_EVERY,
        }
    }
}

impl OverviewService {
    /// `changes` is woken by every relevant home change (`ProcessService::observe_changes`).
    pub fn start(
        source: Arc<dyn OverviewSource>,
        changes: Receiver<()>,
        clock: Clock,
        pacing: Pacing,
    ) -> Result<Arc<Self>, String> {
        let hub = Arc::new(Hub::pending());
        let (wake, inbox) = mpsc::sync_channel(16);
        let forward = wake.clone();
        thread::Builder::new()
            .name("riwork-dashboard-changes".into())
            .spawn(move || {
                while changes.recv().is_ok() {
                    if matches!(
                        forward.try_send(Wake::Changed),
                        Err(mpsc::TrySendError::Disconnected(_))
                    ) {
                        break;
                    }
                }
            })
            .map_err(crate::service::io_error)?;
        let reader_hub = hub.clone();
        thread::Builder::new()
            .name("riwork-dashboard".into())
            .spawn(move || run(source, inbox, reader_hub, clock, pacing))
            .map_err(crate::service::io_error)?;
        Ok(Arc::new(Self { hub, wake }))
    }

    pub fn subscribe(&self) -> TopicSubscription<DashboardDto> {
        let subscription = self.hub.subscribe();
        let _ = self.wake.try_send(Wake::Subscribed);
        subscription
    }

    /// Read everything again now; a request made during a read runs after it.
    pub fn refresh(&self) {
        // A full queue already holds a wake that will read again.
        let _ = self.wake.try_send(Wake::Refresh);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn has_subscribers(&self) -> bool {
        self.hub.has_subscribers()
    }
}

/// When the reader should look next: `None` blocks until a wake.
fn next_deadline(
    subscribed: bool,
    dirty: bool,
    last_read: Option<Instant>,
    now: Instant,
    pacing: Pacing,
) -> Option<Instant> {
    if !subscribed {
        return None;
    }
    match last_read {
        None => Some(now),
        Some(last) if dirty => Some(last + pacing.change_gap),
        Some(last) => Some(last + pacing.extras_every),
    }
}

fn run(
    source: Arc<dyn OverviewSource>,
    inbox: Receiver<Wake>,
    hub: Arc<Hub<DashboardDto>>,
    clock: Clock,
    pacing: Pacing,
) {
    let mut last_read: Option<Instant> = None;
    let mut last_extras: Option<Instant> = None;
    let mut extras: Option<Extras> = None;
    let mut shown: Option<(u64, Read)> = None;
    let mut dirty = true;
    let mut force = false;
    loop {
        let now = Instant::now();
        let deadline = if force {
            Some(now)
        } else {
            next_deadline(hub.has_subscribers(), dirty, last_read, now, pacing)
        };
        let wake = match deadline {
            None => match inbox.recv() {
                Ok(wake) => Some(wake),
                Err(_) => return,
            },
            Some(deadline) if deadline <= now => None,
            Some(deadline) => match inbox.recv_timeout(deadline - now) {
                Ok(wake) => Some(wake),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
        };
        for wake in wake
            .into_iter()
            .chain(std::iter::from_fn(|| inbox.try_recv().ok()))
        {
            match wake {
                Wake::Changed => dirty = true,
                Wake::Refresh => force = true,
                Wake::Subscribed => {}
            }
        }
        if !hub.has_subscribers() {
            // Nobody looks: read again on the next subscription.
            dirty = true;
            continue;
        }
        let now = Instant::now();
        let due = force
            || last_read.is_none_or(|last| {
                now >= last + pacing.extras_every || (dirty && now >= last + pacing.change_gap)
            });
        if !due {
            continue;
        }
        let extras_due = force || last_extras.is_none_or(|last| now >= last + pacing.extras_every);
        let at = clock();
        let result = read_once(source.as_ref(), &mut extras, extras_due, at);
        if extras_due {
            last_extras = Some(now);
        }
        last_read = Some(now);
        dirty = false;
        force = false;
        let error = match result {
            Ok(read) => {
                shown = Some((at, read));
                None
            }
            Err(error) => {
                // Tried again at the change pace: "RiWork tries again every two seconds".
                dirty = true;
                Some(error)
            }
        };
        let dto = DashboardDto {
            schema: 3,
            now: at,
            read_at: shown.as_ref().map(|(read_at, _)| *read_at),
            error,
            wire: shown.as_ref().map(|(_, read)| read.wire.clone()),
            backend_lines: shown
                .as_ref()
                .and_then(|(_, read)| read.backend_lines.clone()),
        };
        hub.publish(dto);
    }
}

static SERVICE: Mutex<Option<Arc<OverviewService>>> = Mutex::new(None);

/// Install a service before the first subscription (bridge tests).
pub fn install_service(service: Arc<OverviewService>) -> Result<(), String> {
    let mut slot = SERVICE.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_some() {
        return Err("conflict: the dashboard service is already running".into());
    }
    *slot = Some(service);
    Ok(())
}

/// The process's service, started on first use. Called on the blocking pool.
pub fn service() -> Result<Arc<OverviewService>, String> {
    let mut slot = SERVICE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(service) = slot.as_ref() {
        return Ok(service.clone());
    }
    let process = crate::service::initialized()?;
    let (changes, inbox) = mpsc::sync_channel(1);
    process.observe_changes(changes);
    let service = OverviewService::start(
        default_source(process.home().to_path_buf())?,
        inbox,
        Arc::new(activity::unix_now),
        Pacing::default(),
    )?;
    *slot = Some(service.clone());
    Ok(service)
}

fn default_source(home: PathBuf) -> Result<Arc<dyn OverviewSource>, String> {
    #[cfg(any(debug_assertions, feature = "test-support"))]
    if riwork_nsh_workspace::session_io::is_fixture(&home) {
        return Ok(Arc::new(fixture::FixtureSource {
            home,
            runtime: tokio::runtime::Handle::try_current().ok(),
        }));
    }
    Ok(Arc::new(EngineSource::new(home)))
}

/// An engine shell as the native inventory describes it.
pub fn engine_shell(dto: &crate::dto::ShellSessionDto) -> ShellSession {
    use riwork_engine::sessions::{HarnessKind, ShellKind};
    ShellSession {
        user_opened: dto.user_opened,
        parent_id: dto.parent_id.clone(),
        id: dto.id.clone(),
        project_id: dto.project_id.clone(),
        worktree_id: dto.worktree_id.clone(),
        kind: if dto.kind == "orchestrator" {
            ShellKind::Orchestrator
        } else {
            ShellKind::Project
        },
        cwd: PathBuf::from(&dto.cwd),
        command: dto.command.clone(),
        editor_path: dto.editor_path.as_ref().map(PathBuf::from),
        harness: dto.harness.as_deref().and_then(|program| match program {
            "codex" => Some(HarnessKind::Codex),
            "claude" => Some(HarnessKind::Claude),
            "grok" => Some(HarnessKind::Grok),
            "muse" => Some(HarnessKind::Muse),
            _ => None,
        }),
        unrestricted: dto.unrestricted,
        codex_account_id: dto.codex_account_id.clone(),
        codex_account_label: dto.codex_account_label.clone(),
        codex_account_email: dto.codex_account_email.clone(),
        codex_home: dto.codex_home.as_ref().map(PathBuf::from),
        orchestrator_skill_loaded: dto.orchestrator_skill_loaded,
        orchestrator_skill_version: dto.orchestrator_skill_version.clone(),
        orchestrator_delegation_version: dto.orchestrator_delegation_version.clone(),
        orchestrator_project_root: dto.orchestrator_project_root.as_ref().map(PathBuf::from),
        created_at_unix: dto.created_at_unix,
        alive: dto.alive,
    }
}

#[cfg(any(debug_assertions, feature = "test-support"))]
pub mod fixture {
    //! Isolated debug fixture homes: shells come from the home's
    //! `fake-sessions.json` (no tmux); every other input is the engine's own
    //! read of the home.
    use super::*;
    use riwork_engine::sessions::ShellKind;

    pub struct FixtureSource {
        pub home: PathBuf,
        /// The fixture inventory schedules its delayed rows on Tokio.
        pub runtime: Option<tokio::runtime::Handle>,
    }

    impl OverviewSource for FixtureSource {
        fn home(&self) -> &Path {
            &self.home
        }
        fn shells(&self) -> Result<ShellSample, String> {
            let _context = self.runtime.as_ref().map(|runtime| runtime.enter());
            let shells = riwork_nsh_workspace::session_io::list(&self.home)?;
            Ok(ShellSample {
                shells: shells
                    .into_iter()
                    .map(|shell| ShellSession {
                        user_opened: shell.user_opened,
                        parent_id: shell.parent_id,
                        id: shell.id,
                        project_id: shell.project_id,
                        worktree_id: shell.worktree_id,
                        kind: if shell.kind == "orchestrator" {
                            ShellKind::Orchestrator
                        } else {
                            ShellKind::Project
                        },
                        cwd: shell.cwd,
                        command: None,
                        editor_path: None,
                        harness: None,
                        unrestricted: false,
                        codex_account_id: None,
                        codex_account_label: None,
                        codex_account_email: None,
                        codex_home: None,
                        orchestrator_skill_loaded: false,
                        orchestrator_skill_version: None,
                        orchestrator_delegation_version: None,
                        orchestrator_project_root: None,
                        created_at_unix: 0,
                        alive: shell.alive,
                    })
                    .collect(),
                ..ShellSample::default()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reader_waits_without_a_subscriber_and_paces_changes_and_heartbeats() {
        let now = Instant::now();
        let pacing = Pacing::default();
        assert_eq!(next_deadline(false, true, None, now, pacing), None);
        assert_eq!(next_deadline(true, true, None, now, pacing), Some(now));
        assert_eq!(
            next_deadline(true, true, Some(now), now, pacing),
            Some(now + CHANGE_GAP)
        );
        assert_eq!(
            next_deadline(true, false, Some(now), now, pacing),
            Some(now + EXTRAS_EVERY)
        );
    }
}
