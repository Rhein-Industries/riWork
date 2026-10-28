//! Host-owned process services: finished-agent notifications and orchestrator
//! wakes (contracts §2.5; parity SHELL-232, SHELL-269..273).
//!
//! One process-wide `notify` watcher on RIWORK_HOME (non-recursive) drives
//! both. Completions are claimed once at startup and whenever
//! `agent-notifications.json` changes. Orchestrator wakes tick when
//! `orchestrator-wakes.json` changes and, only while it holds a pending wake,
//! once a second (a pending wake waits for an idle orchestrator; the engine
//! expires it after `WAKE_LIFETIME`). Nothing runs on a timer while nothing is
//! pending. Engine semantics are unchanged: the claim and the `Runner` are the
//! engine's own.
use riwork_engine::{
    notifications::{self, CompletionNotice},
    orchestrator_wakes::Runner,
    sessions::SessionManager,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, PoisonError, mpsc},
    thread,
    time::{Duration, Instant},
};

const AGENT_LEDGER: &str = "agent-notifications.json";
const WAKE_LEDGER: &str = "orchestrator-wakes.json";
/// The engine refuses larger ledgers; never read more than it would.
const MAX_WAKE_LEDGER: u64 = 8 * 1024 * 1024;
pub(crate) const FIXTURE_ENV: &str = "RIWORK_NSH_NOTIFICATION_FIXTURE";
const FIXTURE_FILE: &str = "fixture-notifications.jsonl";
const WAKE_TICK: Duration = Duration::from_secs(1);
const MAX_TEXT: usize = 1024;
const MAX_TARGET: usize = 4096;
const MAX_ACTIONS: usize = 4;
pub(crate) const OPEN_ACTION: &str = "open";

/// One notification as the host posts it: a finished agent (from the engine's
/// `CompletionNotice::presentation`) or a TSX `system.notify` call.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Notification {
    pub tag: String,
    pub title: String,
    pub body: String,
    pub actions: Vec<NotificationAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct NotificationAction {
    pub id: String,
    pub label: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NotifyRequest {
    id: String,
    title: String,
    body: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    target: Option<Value>,
    #[serde(default)]
    actions: Vec<NotifyAction>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NotifyAction {
    id: String,
    title: String,
}

fn bounded_text(field: &str, value: String) -> Result<String, String> {
    if value.trim().is_empty() || value.len() > MAX_TEXT || value.contains('\0') {
        return Err(format!(
            "invalid: {field} must be a non-empty string of at most {MAX_TEXT} bytes"
        ));
    }
    Ok(value)
}

impl Notification {
    pub(crate) fn completion(notice: &CompletionNotice) -> Self {
        let presentation = notice.presentation();
        Self {
            tag: presentation.tag,
            title: presentation.title,
            body: presentation.body,
            actions: presentation
                .actions
                .into_iter()
                .map(|action| NotificationAction {
                    id: action.id,
                    label: action.label,
                })
                .collect(),
            project_id: Some(notice.project_id.clone()),
            target: Some(json!({"projectId": notice.project_id, "shellId": notice.shell_id})),
        }
    }

    /// `system.notify` (contracts §2.3): `{ id, title, body, projectId?, target?,
    /// actions?: [{ id, title }] }`. id/title/body are non-empty and at most
    /// 1 KiB each; at most four actions.
    pub(crate) fn from_call(data: &Value) -> Result<Self, String> {
        let request = NotifyRequest::deserialize(data).map_err(|e| format!("invalid: {e}"))?;
        if request.actions.len() > MAX_ACTIONS {
            return Err(format!("invalid: at most {MAX_ACTIONS} actions"));
        }
        if request
            .target
            .as_ref()
            .is_some_and(|target| target.to_string().len() > MAX_TARGET)
        {
            return Err(format!("invalid: target exceeds {MAX_TARGET} bytes"));
        }
        Ok(Self {
            tag: bounded_text("id", request.id)?,
            title: bounded_text("title", request.title)?,
            body: bounded_text("body", request.body)?,
            actions: request
                .actions
                .into_iter()
                .map(|action| {
                    Ok(NotificationAction {
                        id: bounded_text("action id", action.id)?,
                        label: bounded_text("action title", action.title)?,
                    })
                })
                .collect::<Result<_, String>>()?,
            project_id: request
                .project_id
                .map(|id| bounded_text("projectId", id))
                .transpose()?,
            target: request.target,
        })
    }
}

/// The `notification.activate` event data (contracts §2.4): `{ id, target? }`,
/// plus `action` when a named action (not the notification itself) was chosen.
pub(crate) fn activation(id: &str, action: Option<&str>, target: Option<Value>) -> Value {
    let mut event = json!({"id": id});
    if let Some(target) = target {
        event["target"] = target;
    }
    if let Some(action) = action {
        event["action"] = json!(action);
    }
    event
}

/// A click on a finished-agent notification itself (`action == None`) or on
/// its "open" action opens that agent; every other action is ignored. The tag
/// must be `riwork-agent-done:<project>:<shell>:<completion>` with canonical
/// UUIDs (engine `response_target`).
pub(crate) fn completion_response(tag: &str, action: Option<&str>) -> Option<(String, String)> {
    if action.is_some_and(|action| action != OPEN_ACTION) {
        return None;
    }
    notifications::response_target(tag)
        .map(|(project_id, shell_id)| (project_id.to_owned(), shell_id.to_owned()))
}

/// The agent must still belong to the project the notification named
/// (SHELL-271; GPUI `open_completed_agent`). Reads the sessions registry.
pub(crate) fn verify_agent(home: &Path, project_id: &str, shell_id: &str) -> Result<(), String> {
    let session = SessionManager::at(home.to_path_buf())?.registered_session(shell_id)?;
    if session.project_id.as_deref() != Some(project_id) {
        return Err("This agent no longer belongs to that project".into());
    }
    Ok(())
}

/// Where notifications go. Implementations must return at once.
pub(crate) trait Notifier: Send + Sync {
    fn post(&self, notification: &Notification) -> Result<(), String>;
}

/// Only an isolated `/tmp` RIWORK_HOME may stand in for the notification
/// center. The host canonicalizes the home, so `/private/tmp` counts too.
fn isolated_home(home: &Path) -> bool {
    home.is_absolute()
        && !home
            .components()
            .any(|component| component == Component::ParentDir)
        && (home.starts_with("/tmp") || home.starts_with("/private/tmp"))
}

/// Acceptance stand-in: one JSON line per notification in
/// `<home>/nsh/fixture-notifications.jsonl`; never the real center.
pub(crate) struct FixtureNotifier {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FixtureNotifier {
    pub(crate) fn at(home: &Path) -> Option<Self> {
        isolated_home(home).then(|| Self {
            path: home.join("nsh").join(FIXTURE_FILE),
            lock: Mutex::new(()),
        })
    }
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Notifier for FixtureNotifier {
    fn post(&self, notification: &Notification) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut line = serde_json::to_vec(notification).map_err(|e| format!("encode: {e}"))?;
        line.push(b'\n');
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("io: {e}"))?;
        }
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut file| file.write_all(&line))
            .map_err(|e| format!("io: {e}"))
    }
}

/// Hands each notification to the main thread, which owns the center.
pub(crate) struct MainThreadNotifier(Box<dyn Fn(Notification) + Send + Sync>);

impl MainThreadNotifier {
    pub(crate) fn new(post: impl Fn(Notification) + Send + Sync + 'static) -> Self {
        Self(Box::new(post))
    }
}

impl Notifier for MainThreadNotifier {
    fn post(&self, notification: &Notification) -> Result<(), String> {
        (self.0)(notification.clone());
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Debug or `terminal-fixture` builds with RIWORK_NSH_NOTIFICATION_FIXTURE=1
    /// and an isolated `/tmp` home: the jsonl stand-in.
    Fixture,
    /// An app-bundle process: UNUserNotificationCenter. macOS refuses to post
    /// from any other binary, so an unbundled build must not claim alerts.
    System,
    /// Nothing claims completions; `system.notify` is unavailable.
    Off,
}

pub(crate) fn delivery(
    home: &Path,
    fixture_flag: Option<&OsStr>,
    executable: Option<&Path>,
) -> Delivery {
    let fixture_build = cfg!(debug_assertions) || cfg!(feature = "terminal-fixture");
    if fixture_build && fixture_flag == Some(OsStr::new("1")) && isolated_home(home) {
        Delivery::Fixture
    } else if executable.is_some_and(notifications::in_app_bundle) {
        Delivery::System
    } else {
        Delivery::Off
    }
}

/// Logs each distinct error once, until the next success.
#[derive(Default)]
pub(crate) struct ErrorOnce(Option<String>);

impl ErrorOnce {
    fn report(&mut self, prefix: &str, error: String) {
        if self.0.as_ref() != Some(&error) {
            eprintln!("{prefix}: {error}");
            self.0 = Some(error);
        }
    }
    fn clear(&mut self) {
        self.0 = None;
    }
}

/// One claim of the engine ledger: every fresh, still-enabled completion is
/// claimed (durably, across processes) and posted. Returns the number posted.
pub(crate) fn claim_and_post(
    home: &Path,
    notifier: &dyn Notifier,
    errors: &mut ErrorOnce,
) -> usize {
    match notifications::claim_pending(home) {
        Ok(notices) => {
            errors.clear();
            let mut posted = 0;
            for notice in &notices {
                match notifier.post(&Notification::completion(notice)) {
                    Ok(()) => posted += 1,
                    Err(error) => errors.report("riwork notifications", error),
                }
            }
            posted
        }
        Err(error) => {
            errors.report("riwork notifications", error);
            0
        }
    }
}

/// What the wake ledger holds, as far as scheduling cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WakeLedger {
    Absent,
    /// Present, but nothing waits for delivery (or it cannot be read here; the
    /// tick after a change surfaces the engine's own error once).
    Settled,
    /// At least one wake waits for an idle orchestrator.
    Pending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WakeSchedule {
    /// Wait for the next watcher event; no timer.
    Block,
    TickAt(Instant),
}

/// Ticks follow a change of the ledger, and repeat once a second only while a
/// wake is pending. Never more than one tick a second.
pub(crate) fn wake_schedule(
    ledger: WakeLedger,
    changed: bool,
    last_tick: Option<Instant>,
    now: Instant,
) -> WakeSchedule {
    let next = WakeSchedule::TickAt(last_tick.map_or(now, |last| last + WAKE_TICK));
    match ledger {
        WakeLedger::Absent => WakeSchedule::Block,
        WakeLedger::Pending => next,
        WakeLedger::Settled if changed => next,
        WakeLedger::Settled => WakeSchedule::Block,
    }
}

pub(crate) fn read_wake_ledger(home: &Path) -> WakeLedger {
    let path = home.join(WAKE_LEDGER);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return WakeLedger::Absent,
        Err(_) => return WakeLedger::Settled,
        Ok(meta) if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_WAKE_LEDGER => {
            return WakeLedger::Settled;
        }
        Ok(_) => {}
    }
    let pending = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|ledger| {
            ledger["entries"].as_array().is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry["state"].as_str() == Some("pending"))
            })
        });
    if pending {
        WakeLedger::Pending
    } else {
        WakeLedger::Settled
    }
}

/// Which ledgers a watcher event touched: (agent notifications, wakes).
fn touched(paths: &[PathBuf]) -> (bool, bool) {
    paths.iter().fold((false, false), |(agent, wake), path| {
        let name = path.file_name();
        (
            agent || name == Some(OsStr::new(AGENT_LEDGER)),
            wake || name == Some(OsStr::new(WAKE_LEDGER)),
        )
    })
}

/// Keeps the home watcher alive for the process.
pub(crate) struct Background {
    _watcher: Option<notify::RecommendedWatcher>,
}

/// Starts the process-wide services once. `notifier` is `None` when this
/// process must not claim completions (`Delivery::Off`).
pub(crate) fn start(home: PathBuf, notifier: Option<Arc<dyn Notifier>>) -> Background {
    use notify::{EventKind, RecursiveMode, Watcher};
    let (agent_tx, agent_rx) = mpsc::sync_channel::<()>(1);
    let (wake_tx, wake_rx) = mpsc::sync_channel::<()>(1);
    let (watch_agent, watch_wake) = (agent_tx.clone(), wake_tx.clone());
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let (agent, wake) = match &event {
            Ok(event) if event.need_rescan() => (true, true),
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => (false, false),
            Ok(event) => touched(&event.paths),
            Err(_) => (true, true),
        };
        if agent {
            let _ = watch_agent.try_send(());
        }
        if wake {
            let _ = watch_wake.try_send(());
        }
    })
    .and_then(|mut watcher| {
        watcher.watch(&home, RecursiveMode::NonRecursive)?;
        Ok(watcher)
    });
    let watcher = match watcher {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            // Without the push source, fall back to GPUI's once-a-second poll
            // rather than never delivering.
            eprintln!("riwork notifications: home watcher unavailable, polling: {error}");
            let _ = thread::Builder::new()
                .name("riwork-home-poll".into())
                .spawn(move || {
                    loop {
                        thread::sleep(WAKE_TICK);
                        let agent = agent_tx.try_send(());
                        let wake = wake_tx.try_send(());
                        if matches!(agent, Err(mpsc::TrySendError::Disconnected(_)))
                            && matches!(wake, Err(mpsc::TrySendError::Disconnected(_)))
                        {
                            return;
                        }
                    }
                });
            None
        }
    };
    if let Some(notifier) = notifier {
        let home = home.clone();
        let _ = thread::Builder::new()
            .name("riwork-notifications".into())
            .spawn(move || {
                let mut errors = ErrorOnce::default();
                loop {
                    claim_and_post(&home, &*notifier, &mut errors);
                    if agent_rx.recv().is_err() {
                        return;
                    }
                    while agent_rx.try_recv().is_ok() {}
                }
            });
    }
    let _ = thread::Builder::new()
        .name("riwork-orchestrator-wakes".into())
        .spawn(move || run_wakes(home, wake_rx));
    Background { _watcher: watcher }
}

/// One `Runner` for the process, kept across ticks (its rollout readers persist).
fn run_wakes(home: PathBuf, changes: mpsc::Receiver<()>) {
    let mut runner = Runner::at(home.clone());
    let mut errors = ErrorOnce::default();
    let mut changed = true;
    let mut last_tick = None;
    loop {
        let now = Instant::now();
        match wake_schedule(read_wake_ledger(&home), changed, last_tick, now) {
            WakeSchedule::Block => match changes.recv() {
                Ok(()) => changed = true,
                Err(_) => return,
            },
            WakeSchedule::TickAt(at) if at > now => match changes.recv_timeout(at - now) {
                Ok(()) => changed = true,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            },
            WakeSchedule::TickAt(_) => {
                changed = false;
                match runner.tick() {
                    Ok(()) => errors.clear(),
                    Err(error) => errors.report("riwork orchestrator wakes", error),
                }
                last_tick = Some(Instant::now());
            }
        }
    }
}

/// The production poster: UNUserNotificationCenter (ported from GPUI's
/// `gpui-pre-macos` `system_notifications.rs`, which uses the same 0.3
/// bindings). Main thread only; created at startup so a click that launches or
/// activates the app reaches the delegate.
pub(crate) use center::NotificationCenter;
mod center {
    use super::{Notification, NotificationAction};
    use block2::RcBlock;
    use nsh_platform_macos::{
        objc2::{
            AnyThread, DefinedClass, define_class, msg_send,
            rc::Retained,
            runtime::{Bool, ProtocolObject},
        },
        objc2_foundation::{
            NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSSet, NSString,
        },
    };
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction,
        UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
        UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
        UNNotificationRequest, UNNotificationResponse, UNUserNotificationCenter,
        UNUserNotificationCenterDelegate,
    };
    use std::{
        cell::{Cell, RefCell},
        collections::HashMap,
        sync::Arc,
    };

    /// (tag, action id; `None` for the notification itself). Any thread.
    pub(crate) type Respond = Arc<dyn Fn(String, Option<String>) + Send + Sync>;

    pub(crate) struct NotificationCenter {
        center: Retained<UNUserNotificationCenter>,
        /// The center's delegate is weak; this keeps responses flowing.
        _delegate: Retained<ResponseDelegate>,
        /// Every action set registered so far: macOS replaces the whole set
        /// whenever a category is added.
        categories:
            RefCell<HashMap<Vec<NotificationAction>, (String, Retained<UNNotificationCategory>)>>,
        authorization_requested: Cell<bool>,
    }

    impl NotificationCenter {
        pub(crate) fn new(respond: Respond) -> Option<Self> {
            // UNUserNotificationCenter aborts the process outside an app bundle.
            if NSBundle::mainBundle().bundleIdentifier().is_none() {
                eprintln!("riwork notifications: disabled outside an app bundle");
                return None;
            }
            let center = UNUserNotificationCenter::currentNotificationCenter();
            let delegate = ResponseDelegate::new(respond);
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            Some(Self {
                center,
                _delegate: delegate,
                categories: RefCell::new(HashMap::new()),
                authorization_requested: Cell::new(false),
            })
        }

        fn request_authorization(&self) {
            if self.authorization_requested.replace(true) {
                return;
            }
            let completion = RcBlock::new(|granted: Bool, error: *mut NSError| {
                // SAFETY: a non-null error is valid for the callback's duration.
                if let Some(error) = unsafe { error.as_ref() } {
                    eprintln!(
                        "riwork notifications: authorization failed: {}",
                        error.localizedDescription()
                    );
                } else if !granted.as_bool() {
                    eprintln!("riwork notifications: authorization denied");
                }
            });
            self.center
                .requestAuthorizationWithOptions_completionHandler(
                    UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                    &completion,
                );
        }

        pub(crate) fn post(&self, notification: &Notification) {
            self.request_authorization();
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&notification.title));
            content.setBody(&NSString::from_str(&notification.body));
            if !notification.actions.is_empty() {
                let category = self.category(&notification.actions);
                content.setCategoryIdentifier(&NSString::from_str(&category));
            }
            // A nil trigger delivers now; the tag as identifier lets a newer
            // notification with the same tag replace the older one.
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(&notification.tag),
                &content,
                None,
            );
            let completion = RcBlock::new(|error: *mut NSError| {
                // SAFETY: a non-null error is valid for the callback's duration.
                if let Some(error) = unsafe { error.as_ref() } {
                    eprintln!(
                        "riwork notifications: delivery failed: {}",
                        error.localizedDescription()
                    );
                }
            });
            self.center
                .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
        }

        fn category(&self, actions: &[NotificationAction]) -> String {
            let mut categories = self.categories.borrow_mut();
            if let Some((identifier, _)) = categories.get(actions) {
                return identifier.clone();
            }
            let identifier = format!("riwork-notification-{}", categories.len());
            let platform: Vec<Retained<UNNotificationAction>> = actions
                .iter()
                .map(|action| {
                    UNNotificationAction::actionWithIdentifier_title_options(
                        &NSString::from_str(&action.id),
                        &NSString::from_str(&action.label),
                        UNNotificationActionOptions::Foreground,
                    )
                })
                .collect();
            let category =
                UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                    &NSString::from_str(&identifier),
                    &NSArray::from_retained_slice(&platform),
                    &NSArray::new(),
                    UNNotificationCategoryOptions::empty(),
                );
            categories.insert(actions.to_vec(), (identifier.clone(), category));
            let all: Vec<Retained<UNNotificationCategory>> = categories
                .values()
                .map(|(_, category)| category.clone())
                .collect();
            self.center
                .setNotificationCategories(&NSSet::from_retained_slice(&all));
            identifier
        }
    }

    pub(crate) struct DelegateIvars {
        respond: Respond,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements and this class has no Drop.
        #[unsafe(super(NSObject))]
        #[ivars = DelegateIvars]
        #[name = "RiWorkNshNotificationDelegate"]
        pub(crate) struct ResponseDelegate;

        unsafe impl NSObjectProtocol for ResponseDelegate {}

        unsafe impl UNUserNotificationCenterDelegate for ResponseDelegate {
            // Possibly on a non-main thread; `respond` hops to the main queue.
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                completion: &block2::DynBlock<dyn Fn()>,
            ) {
                let tag = response.notification().request().identifier().to_string();
                let action = response.actionIdentifier();
                let action = if &*action == unsafe { UNNotificationDefaultActionIdentifier } {
                    None
                } else {
                    Some(action.to_string())
                };
                (self.ivars().respond)(tag, action);
                completion.call(());
            }

            // Without this, macOS hides banners while RiWork is frontmost.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                completion.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List,));
            }
        }
    );

    impl ResponseDelegate {
        fn new(respond: Respond) -> Retained<Self> {
            let this = Self::alloc().set_ivars(DelegateIvars { respond });
            // SAFETY: NSObject's init is its designated initializer.
            unsafe { msg_send![super(this), init] }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riwork_core::store::Store;
    use riwork_engine::sessions::{HarnessKind, ShellKind, ShellSession};
    use uuid::Uuid;

    struct Home(PathBuf);
    impl Home {
        fn new() -> Self {
            let root = PathBuf::from("/tmp").join(format!("riwork-nsh-notify-{}", Uuid::new_v4()));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct Recording(Mutex<Vec<Notification>>);
    impl Notifier for Recording {
        fn post(&self, notification: &Notification) -> Result<(), String> {
            self.0.lock().unwrap().push(notification.clone());
            Ok(())
        }
    }

    /// An opted-in project with one registered Codex agent, written with the
    /// engine's and core's own writers (as `riwork_engine::notifications` tests do).
    fn opted_in_agent(home: &Path) -> (String, String) {
        fs::create_dir_all(home.join("project")).unwrap();
        let store = Store::open(home.to_path_buf()).unwrap();
        let project = store
            .add_project(home.join("project"), Some("Project A"))
            .unwrap();
        store.set_project_notifications(&project.id, true).unwrap();
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
            orchestrator_delegation_version: None,
            orchestrator_project_root: None,
            created_at_unix: 1,
            alive: true,
        };
        fs::write(
            home.join("sessions.json"),
            json!({"sessions": [shell]}).to_string(),
        )
        .unwrap();
        (project.id, shell_id)
    }

    #[test]
    fn a_pending_completion_is_posted_once_with_the_engine_presentation() {
        let home = Home::new();
        let (project, shell) = opted_in_agent(&home.0);
        assert!(
            notifications::record_completion(&home.0, &shell, "thread:turn-1", HarnessKind::Codex)
                .unwrap()
        );
        let notifier = Recording::default();
        let mut errors = ErrorOnce::default();
        assert_eq!(claim_and_post(&home.0, &notifier, &mut errors), 1);
        let posted = notifier.0.lock().unwrap().clone();
        assert_eq!(posted.len(), 1);
        let alert = &posted[0];
        let prefix = format!("riwork-agent-done:{project}:{shell}:");
        assert!(alert.tag.starts_with(&prefix), "{}", alert.tag);
        assert!(Uuid::parse_str(&alert.tag[prefix.len()..]).is_ok());
        assert_eq!(alert.title, "Project A · Codex finished");
        assert_eq!(
            alert.body,
            format!("Agent {} completed its turn.", &shell[..8])
        );
        assert_eq!(
            alert.actions,
            vec![NotificationAction {
                id: "open".into(),
                label: "Open agent".into()
            }]
        );
        assert_eq!(alert.project_id.as_deref(), Some(project.as_str()));
        assert_eq!(
            alert.target,
            Some(json!({"projectId": project, "shellId": shell}))
        );
        // Claimed once: the durable claim survives, so nothing is posted twice.
        assert_eq!(claim_and_post(&home.0, &notifier, &mut errors), 0);
        assert_eq!(notifier.0.lock().unwrap().len(), 1);
        assert!(errors.0.is_none());
    }

    #[test]
    fn the_response_maps_the_tag_back_and_ignores_other_actions() {
        let (project, shell, completion) = (
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
        );
        let tag = format!("riwork-agent-done:{project}:{shell}:{completion}");
        let expected = Some((project.clone(), shell.clone()));
        assert_eq!(completion_response(&tag, None), expected);
        assert_eq!(completion_response(&tag, Some("open")), expected);
        assert_eq!(completion_response(&tag, Some("snooze")), None);
        assert_eq!(
            completion_response(&format!("riwork-notifications-enabled:{project}"), None),
            None
        );
        assert_eq!(
            completion_response(
                &tag.to_uppercase()
                    .replace("RIWORK-AGENT-DONE", "riwork-agent-done"),
                None
            ),
            None,
            "ids must be canonical UUIDs"
        );
        assert_eq!(
            activation(
                &tag,
                None,
                Some(json!({"projectId": project, "shellId": shell}))
            ),
            json!({"id": tag, "target": {"projectId": project, "shellId": shell}})
        );
        assert_eq!(
            activation("custom", Some("open"), None),
            json!({"id": "custom", "action": "open"})
        );
    }

    #[test]
    fn the_response_verifies_the_agent_still_belongs_to_the_project() {
        let home = Home::new();
        let (project, shell) = opted_in_agent(&home.0);
        assert_eq!(verify_agent(&home.0, &project, &shell), Ok(()));
        assert_eq!(
            verify_agent(&home.0, &Uuid::new_v4().to_string(), &shell),
            Err("This agent no longer belongs to that project".into())
        );
        assert!(verify_agent(&home.0, &project, &Uuid::new_v4().to_string()).is_err());
    }

    #[test]
    fn the_fixture_poster_writes_jsonl_only_under_a_tmp_home() {
        let home = Home::new();
        let fixture = FixtureNotifier::at(&home.0).expect("a /tmp home");
        let first = Notification::from_call(&json!({"id":"n1","title":"T","body":"B",
            "projectId":"p","target":{"k":1},"actions":[{"id":"open","title":"Open agent"}]}))
        .unwrap();
        fixture.post(&first).unwrap();
        fixture
            .post(&Notification::from_call(&json!({"id":"n2","title":"T2","body":"B2"})).unwrap())
            .unwrap();
        assert_eq!(
            fixture.path(),
            home.0.join("nsh/fixture-notifications.jsonl")
        );
        let text = fs::read_to_string(fixture.path()).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            lines,
            vec![
                json!({"tag":"n1","title":"T","body":"B","actions":[{"id":"open","label":"Open agent"}],
                    "projectId":"p","target":{"k":1}}),
                json!({"tag":"n2","title":"T2","body":"B2","actions":[]}),
            ]
        );
        for home in [
            "/Users/someone/.local/share/riwork",
            "/tmpfoo/state",
            "/tmp/../Users/someone",
            "relative/tmp",
        ] {
            assert!(FixtureNotifier::at(Path::new(home)).is_none(), "{home}");
        }
        assert!(FixtureNotifier::at(Path::new("/private/tmp/rw/state")).is_some());
        let flag = Some(OsStr::new("1"));
        let bundle = Path::new("/Applications/RiWork.app/Contents/MacOS/riwork");
        let plain = Path::new("/tmp/target/debug/riwork");
        assert_eq!(delivery(&home.0, flag, Some(plain)), Delivery::Fixture);
        assert_eq!(delivery(&home.0, flag, Some(bundle)), Delivery::Fixture);
        assert_eq!(delivery(&home.0, None, Some(bundle)), Delivery::System);
        assert_eq!(delivery(&home.0, None, Some(plain)), Delivery::Off);
        assert_eq!(
            delivery(&home.0, Some(OsStr::new("0")), Some(plain)),
            Delivery::Off
        );
        let real = Path::new("/Users/someone/.local/share/riwork");
        assert_eq!(delivery(real, flag, Some(plain)), Delivery::Off);
        assert_eq!(delivery(real, flag, Some(bundle)), Delivery::System);
    }

    #[test]
    fn system_notify_data_is_validated() {
        let ok = Notification::from_call(&json!({"id":"a","title":"b","body":"c",
            "actions":[{"id":"open","title":"Open"}]}))
        .unwrap();
        assert_eq!(ok.tag, "a");
        assert_eq!(ok.actions[0].label, "Open");
        let long = "x".repeat(1025);
        for bad in [
            json!({"title":"b","body":"c"}),
            json!({"id":"","title":"b","body":"c"}),
            json!({"id":"a","title":"  ","body":"c"}),
            json!({"id":"a","title":long,"body":"c"}),
            json!({"id":"a","title":"b","body":long}),
            json!({"id":1,"title":"b","body":"c"}),
            json!({"id":"a","title":"b","body":"c","extra":true}),
            json!({"id":"a","title":"b","body":"c","actions":[{"id":"","title":"x"}]}),
            json!({"id":"a","title":"b","body":"c","actions":[
                {"id":"1","title":"1"},{"id":"2","title":"2"},{"id":"3","title":"3"},
                {"id":"4","title":"4"},{"id":"5","title":"5"}]}),
        ] {
            assert!(
                Notification::from_call(&bad)
                    .unwrap_err()
                    .starts_with("invalid:"),
                "{bad}"
            );
        }
        assert!(
            Notification::from_call(&json!({"id":"x".repeat(1024),"title":"b","body":"c"})).is_ok()
        );
    }

    #[test]
    fn wakes_block_without_a_pending_wake_and_tick_once_a_second_with_one() {
        let now = Instant::now();
        let last = now - Duration::from_millis(300);
        assert_eq!(
            wake_schedule(WakeLedger::Absent, true, None, now),
            WakeSchedule::Block
        );
        assert_eq!(
            wake_schedule(WakeLedger::Absent, false, Some(last), now),
            WakeSchedule::Block
        );
        assert_eq!(
            wake_schedule(WakeLedger::Settled, false, Some(last), now),
            WakeSchedule::Block
        );
        assert_eq!(
            wake_schedule(WakeLedger::Settled, true, Some(last), now),
            WakeSchedule::TickAt(last + WAKE_TICK)
        );
        assert_eq!(
            wake_schedule(WakeLedger::Settled, true, None, now),
            WakeSchedule::TickAt(now)
        );
        assert_eq!(
            wake_schedule(WakeLedger::Pending, false, None, now),
            WakeSchedule::TickAt(now)
        );
        assert_eq!(
            wake_schedule(WakeLedger::Pending, false, Some(last), now),
            WakeSchedule::TickAt(last + WAKE_TICK)
        );

        let home = Home::new();
        let ledger = home.0.join(WAKE_LEDGER);
        assert_eq!(read_wake_ledger(&home.0), WakeLedger::Absent);
        fs::write(&ledger, "").unwrap();
        assert_eq!(read_wake_ledger(&home.0), WakeLedger::Settled);
        fs::write(
            &ledger,
            r#"{"entries":[{"state":"delivered"},{"state":"expired"}]}"#,
        )
        .unwrap();
        assert_eq!(read_wake_ledger(&home.0), WakeLedger::Settled);
        fs::write(
            &ledger,
            r#"{"entries":[{"state":"delivered"},{"state":"pending"}]}"#,
        )
        .unwrap();
        assert_eq!(read_wake_ledger(&home.0), WakeLedger::Pending);
        fs::write(&ledger, "not json").unwrap();
        assert_eq!(read_wake_ledger(&home.0), WakeLedger::Settled);
    }

    #[test]
    fn only_the_two_ledgers_wake_their_services() {
        let home = Path::new("/tmp/rw/state");
        assert_eq!(touched(&[home.join(AGENT_LEDGER)]), (true, false));
        assert_eq!(touched(&[home.join(WAKE_LEDGER)]), (false, true));
        assert_eq!(
            touched(&[
                home.join(".agent-notifications-1.tmp"),
                home.join("orchestrator-wakes.lock")
            ]),
            (false, false)
        );
        assert_eq!(
            touched(&[
                home.join(".tmp"),
                home.join(AGENT_LEDGER),
                home.join(WAKE_LEDGER)
            ]),
            (true, true)
        );
    }
}
