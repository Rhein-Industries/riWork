//! One metadata subscription service for all windows and mods, with one read in
//! flight per host. Read failures preserve the last good snapshot. No mutations
//! are replayed, and monitoring never launches a provider or terminal.
use crate::{
    Subscription,
    hub::{Hub, TopicSubscription},
    service,
};
use nsh_core::prelude::*;
use riwork_engine::{
    host_sessions::{self as model, Cache, Host, LocalSnapshot},
    remote_hosts::{HostStatus, LinkState, RegisteredHost, RemoteError},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// What a read that produced no snapshot says about the route.
#[derive(Debug, PartialEq, Eq)]
enum ReadFailure {
    /// Retrying cannot help until a person acts. Reads go on, so that a fix is noticed.
    Blocked(String),
    /// The link answered and the host could not report.
    HostError(String),
    /// The link is down or an answer was lost.
    Backoff(String),
    /// The link is still being made, which is no failure at all.
    Connecting,
}

struct State {
    cache: Cache,
    running: BTreeSet<String>,
    due: BTreeMap<String, Instant>,
    failures: BTreeMap<String, u32>,
    registry_error: Option<String>,
    revision: u64,
    saved: u64,
    /// From the last registry read: the identity each SSH route was added for.
    pins: BTreeMap<String, String>,
    /// Counts routes forgotten, so that a registry list read before one is not applied after it.
    forgets: u64,
}
pub struct HostsService {
    home: PathBuf,
    state: Mutex<State>,
    topic: Hub<Value>,
    requested: AtomicBool,
    registry_running: AtomicBool,
    persistence: Mutex<()>,
}
impl HostsService {
    fn snapshot_locked(state: &State) -> Value {
        json!({"version":1,"hosts":state.cache.rows(),"error":state.registry_error})
    }
    pub fn snapshot(&self) -> Value {
        self.requested.store(true, Ordering::Release);
        let mut state = self.state.lock().unwrap();
        self.age_if_unobserved(&mut state);
        Self::snapshot_locked(&state)
    }
    pub fn subscribe(&self) -> TopicSubscription<Value> {
        self.requested.store(true, Ordering::Release);
        // Before the listener registers: subscribing replays the latest value at once.
        self.age_if_unobserved(&mut self.state.lock().unwrap());
        self.topic.subscribe()
    }
    // Reads stop while nobody watches. While someone is subscribed, the reads keep rows honest,
    // and a read can take longer than the age limit, so this runs only with no subscriber: for an
    // observer that is returning, and for a caller that asks without subscribing, whose asking
    // keeps the reads going. Only rows that say the link is up are aged, so such a caller still
    // sees a host that is down as down.
    fn age_if_unobserved(&self, state: &mut State) {
        if !self.topic.has_subscribers()
            && state
                .cache
                .age_unobserved(riwork_engine::activity::unix_now())
        {
            state.revision += 1;
            self.publish(state);
        }
    }
    fn publish(&self, state: &State) {
        self.topic.publish(Self::snapshot_locked(state));
    }
    pub fn seen(&self, host: &str, kind: &str, session: &str, token: &str) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        state.cache.seen(host, kind, session, token)?;
        state.revision += 1;
        self.publish(&state);
        drop(state);
        self.persist()
    }
    fn complete(&self, key: &str, result: Result<LocalSnapshot, ReadFailure>) {
        let mut state = self.state.lock().unwrap();
        state.running.remove(key);
        if !state.cache.hosts.contains_key(key) {
            return;
        }
        // The dial holds an SSH route to the identity it was added for, as the host announces it.
        // The inventory names an identity of its own, and that is what the row shows, so it is
        // held to the same one.
        let pin = state.pins.get(key).cloned();
        let result = result.and_then(|snapshot| {
            if pin.is_some_and(|pin| pin != snapshot.host_id) {
                return Err(ReadFailure::Blocked(NOT_PINNED_HOST.into()));
            }
            state
                .cache
                .apply_read(key, snapshot, riwork_engine::activity::unix_now())
                .map_err(ReadFailure::Blocked)
        });
        let ok = result.is_ok();
        let connecting = matches!(result, Err(ReadFailure::Connecting));
        if let Err(failure) = result
            && let Some(host) = state.cache.hosts.get_mut(key)
        {
            match failure {
                ReadFailure::Blocked(message) => host.failed(message, true),
                ReadFailure::HostError(message) => host.report_failed(message),
                ReadFailure::Backoff(message) => host.failed(message, false),
                ReadFailure::Connecting => host.connecting(),
            }
        }
        let failed = state.failures.entry(key.to_owned()).or_default();
        if !connecting {
            *failed = if ok { 0 } else { (*failed + 1).min(5) };
        }
        let delay = if key == "local" {
            2
        } else if connecting {
            1
        } else if ok {
            5
        } else {
            2u64.pow(*failed).min(60)
        };
        state
            .due
            .insert(key.into(), Instant::now() + Duration::from_secs(delay));
        state.revision += 1;
        self.publish(&state);
    }
    // Serialize writers, but never hold the UI snapshot lock across disk I/O.
    // The coordinator coalesces routine updates; acknowledgements persist immediately.
    fn persist(&self) -> Result<(), String> {
        let _writer = self.persistence.lock().unwrap();
        let (cache, revision) = {
            let state = self.state.lock().unwrap();
            if state.revision == state.saved {
                return Ok(());
            }
            (state.cache.clone(), state.revision)
        };
        cache.save_owned(&self.home)?;
        self.state.lock().unwrap().saved = revision;
        Ok(())
    }
    fn refresh_registry(&self) {
        #[cfg(feature = "test-support")]
        self.refresh_registry_with(|| Ok(vec![]));
        #[cfg(not(feature = "test-support"))]
        self.refresh_registry_with(|| {
            riwork_engine::remote_hosts::RemoteCli::locate()
                .and_then(|cli| cli.hosts_with_transport())
        });
    }
    fn refresh_registry_with(&self, list: impl FnOnce() -> Result<Vec<RegisteredHost>, String>) {
        let forgets = self.state.lock().unwrap().forgets;
        let hosts = list();
        let mut state = self.state.lock().unwrap();
        // A route forgotten while the list ran may still be on it, and would come back. The next
        // list is due within ten seconds.
        if state.forgets != forgets {
            return;
        }
        match hosts {
            Err(e) => state.registry_error = Some(e),
            Ok(hosts) => apply_registry(&mut state, hosts),
        }
        state.revision += 1;
        self.publish(&state);
    }
    // A read in flight finds no row when it completes, and only clears `running`. A registry list
    // in flight is not applied.
    fn forget(&self, id: &str) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if id == "local" {
            return Ok(());
        }
        // Counted with no row too: a list in flight could still make one.
        state.forgets += 1;
        state.pins.remove(id);
        if state.cache.hosts.remove(id).is_none() {
            return Ok(());
        }
        state.due.remove(id);
        state.failures.remove(id);
        state.revision += 1;
        self.publish(&state);
        drop(state);
        self.persist()
    }
}

fn apply_registry(state: &mut State, hosts: Vec<RegisteredHost>) {
    state.registry_error = (hosts.len() > model::MAX_HOSTS)
        .then(|| format!("Showing the first {} hosts", model::MAX_HOSTS));
    let ids: BTreeSet<_> = hosts
        .iter()
        .take(model::MAX_HOSTS)
        .map(|route| route.host.id.clone())
        .chain(["local".into()])
        .collect();
    state.cache.hosts.retain(|id, _| ids.contains(id));
    state.due.retain(|id, _| ids.contains(id));
    state.failures.retain(|id, _| ids.contains(id));
    state.pins = hosts
        .iter()
        .take(model::MAX_HOSTS)
        .filter_map(|route| Some((route.host.id.clone(), route.host_id.clone()?)))
        .collect();
    for RegisteredHost {
        host,
        transport,
        added_at,
        host_id,
    } in hosts.into_iter().take(model::MAX_HOSTS)
    {
        // A relay route removed and paired again keeps its id. The row describes the old record,
        // whose pin says nothing about the new one, and the removal may have gone unseen with this
        // service asleep. A changed identity on the same record stays refused.
        let replaced = state.cache.hosts.get(&host.id).is_some_and(|row| {
            row.added_at.is_some() && added_at.is_some() && row.added_at != added_at
        });
        // A row holding another identity than the route's pin never showed that route's host: it
        // was read before the registry was, or kept from a build that did not compare them.
        let foreign = host_id.is_some_and(|pin| {
            state
                .cache
                .hosts
                .get(&host.id)
                .and_then(|row| row.snapshot.as_ref())
                .is_some_and(|snapshot| snapshot.host_id != pin)
        });
        if replaced || foreign {
            state.cache.hosts.remove(&host.id);
            state.due.remove(&host.id);
            state.failures.remove(&host.id);
        }
        let row = state
            .cache
            .hosts
            .entry(host.id.clone())
            .or_insert_with(|| Host::pending(Some(host.id), host.label.clone(), &transport));
        row.label = model::bounded(&host.label, 256);
        row.route = transport;
        row.added_at = added_at.or(row.added_at);
    }
}

// Reserve one slot for the local host. Slow/offline machines cannot starve it;
// never-read and oldest-due remote hosts win over another retry of an early key.
// Hosts with failures hold at most two of the three remote slots, so that several bad
// ones at once cannot delay a healthy one.
fn due_keys(state: &State, now: Instant) -> Vec<String> {
    let mut due: Vec<_> = state
        .cache
        .hosts
        .keys()
        .filter(|key| {
            !state.running.contains(*key) && state.due.get(*key).is_none_or(|at| *at <= now)
        })
        .cloned()
        .collect();
    due.sort_by_key(|key| (key != "local", state.due.get(key).copied(), key.clone()));
    let mut remote_slots = 3usize.saturating_sub(
        state
            .running
            .iter()
            .filter(|key| key.as_str() != "local")
            .count(),
    );
    let failing =
        |key: &String| key != "local" && state.failures.get(key).is_some_and(|count| *count > 0);
    let mut failing_slots =
        2usize.saturating_sub(state.running.iter().filter(|key| failing(key)).count());
    due.retain(|key| {
        if key == "local" {
            return true;
        }
        if remote_slots == 0 || (failing(key) && failing_slots == 0) {
            return false;
        }
        remote_slots -= 1;
        failing_slots -= usize::from(failing(key));
        true
    });
    due
}

static SERVICES: OnceLock<Mutex<BTreeMap<PathBuf, Arc<HostsService>>>> = OnceLock::new();
pub fn observe(home: &Path) -> Result<Arc<HostsService>, String> {
    let mut services = SERVICES.get_or_init(Default::default).lock().unwrap();
    if let Some(service) = services.get(home) {
        return Ok(service.clone());
    }
    let mut cache = Cache::open(home);
    cache
        .hosts
        .entry("local".into())
        .or_insert_with(|| Host::pending(None, "This Mac".into(), "local"));
    let state = State {
        cache,
        running: BTreeSet::new(),
        due: BTreeMap::new(),
        failures: BTreeMap::new(),
        registry_error: None,
        revision: 1,
        saved: 0,
        pins: BTreeMap::new(),
        forgets: 0,
    };
    let service = Arc::new(HostsService {
        home: home.into(),
        topic: Hub::new(HostsService::snapshot_locked(&state)),
        state: Mutex::new(state),
        requested: AtomicBool::new(true),
        registry_running: AtomicBool::new(false),
        persistence: Mutex::new(()),
    });
    let weak = Arc::downgrade(&service);
    std::thread::Builder::new()
        .name("somtus-hosts".into())
        .spawn(move || {
            let mut registry_at: Option<Instant> = None;
            let mut persisted_at = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(250));
                let Some(service) = weak.upgrade() else { break };
                if persisted_at.elapsed() >= Duration::from_secs(10) {
                    persisted_at = Instant::now();
                    if let Err(error) = service.persist() {
                        let mut state = service.state.lock().unwrap();
                        state.registry_error = Some(format!("Could not save host cache: {error}"));
                        service.publish(&state);
                    }
                }
                let requested = service.requested.swap(false, Ordering::AcqRel);
                if !requested && !service.topic.has_subscribers() {
                    continue;
                }
                if registry_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(10))
                    && !service.registry_running.swap(true, Ordering::AcqRel)
                {
                    // Registry reads don't hold the publication lock or block any host read.
                    registry_at = Some(Instant::now());
                    let copy = service.clone();
                    std::thread::spawn(move || {
                        copy.refresh_registry();
                        copy.registry_running.store(false, Ordering::Release);
                    });
                }
                let keys = {
                    let mut state = service.state.lock().unwrap();
                    let keys = due_keys(&state, Instant::now());
                    state.running.extend(keys.iter().cloned());
                    keys
                };
                for key in keys {
                    let service = service.clone();
                    let copy = service.clone();
                    let failed_key = key.clone();
                    if let Err(error) = std::thread::Builder::new()
                        .name("somtus-host-read".into())
                        .spawn(move || {
                            let result = if key == "local" {
                                model::local(&service.home).map_err(ReadFailure::HostError)
                            } else {
                                remote_snapshot(&key)
                            };
                            service.complete(&key, result);
                        })
                    {
                        copy.complete(&failed_key, Err(ReadFailure::Backoff(error.to_string())));
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    services.insert(home.to_path_buf(), service.clone());
    Ok(service)
}

/// Drops a route the person just removed, from every view and from disk, at once: the next
/// registry read may be a long way off when nothing is watching. With nothing watching yet there
/// is no service, but the file still holds that host's session titles.
pub fn forget_route(home: &Path, id: &str) -> Result<(), String> {
    let service = SERVICES
        .get()
        .and_then(|services| services.lock().unwrap().get(home).cloned());
    if let Some(service) = service {
        return service.forget(id);
    }
    let mut cache = Cache::open(home);
    if id == "local" || cache.hosts.remove(id).is_none() {
        return Ok(());
    }
    cache.save_owned(home)
}

fn remote_snapshot(host: &str) -> Result<LocalSnapshot, ReadFailure> {
    read_remote(crate::remote::host_status(host), || {
        crate::remote::call_host(
            host,
            "native.invoke",
            json!({"module":"Hosts","method":"local","args":{}}),
            Duration::from_secs(20),
        )
    })
}

const NEEDS_UPDATE: &str = "This host needs a Somtus update to report agent sessions";
const NOT_PINNED_HOST: &str = "This host's sessions come from another Somtus installation than the one this route was added for. Check the route's --somtus and --home, or remove the route and add it again.";

/// Asks the link first. A read on a link still being made would wait out its whole limit in one of
/// the few remote slots, and one that is down would only fail with the reason the status has.
fn read_remote(
    status: Result<HostStatus, String>,
    call: impl FnOnce() -> Result<Value, RemoteError>,
) -> Result<LocalSnapshot, ReadFailure> {
    let status = status.map_err(ReadFailure::Backoff)?;
    match status.state {
        LinkState::Connecting => return Err(ReadFailure::Connecting),
        LinkState::Offline => {
            let reason = status
                .reason
                .unwrap_or_else(|| "This host is not connected".into());
            return Err(if status.needs_action {
                ReadFailure::Blocked(reason)
            } else {
                ReadFailure::Backoff(reason)
            });
        }
        LinkState::Online => {}
    }
    let result = call().map_err(|error| classify(&error))?;
    serde_json::from_value(result.get("value").cloned().unwrap_or(result))
        .map_err(|_| ReadFailure::Blocked(NEEDS_UPDATE.into()))
}

fn classify(error: &RemoteError) -> ReadFailure {
    match error {
        _ if error.is_unsupported() => ReadFailure::Blocked(NEEDS_UPDATE.into()),
        RemoteError::NeedsSetup { .. } => ReadFailure::Blocked(error.message()),
        RemoteError::Rpc { code, .. } if code == "response_too_large" => {
            ReadFailure::Blocked(error.message())
        }
        RemoteError::Rpc { code, .. } if code == "cli_error" => {
            ReadFailure::HostError(error.message())
        }
        _ => ReadFailure::Backoff(error.message()),
    }
}

/// Connection health and agent metadata across execution hosts.
#[module]
pub struct Hosts;
#[module]
impl Hosts {
    /// Read only this execution host; never starts a provider or terminal.
    pub async fn local() -> Result<Value, String> {
        service::blocking(|| {
            serde_json::to_value(model::local(&service::process_home()?)?)
                .map_err(|e| e.to_string())
        })
        .await
    }
    /// Return cached all-host metadata and schedule a refresh.
    pub async fn snapshot() -> Result<Value, String> {
        service::blocking(|| Ok(observe(&service::process_home()?)?.snapshot())).await
    }
    /// Observe revisioned snapshots; a slow consumer receives a replacement.
    pub fn subscribe(listener: Callback<Value>, cx: CallCx<'_>) -> Subscription {
        Subscription::start(cx, async move {
            if let Ok(subscription) =
                service::blocking(|| Ok(observe(&service::process_home()?)?.subscribe())).await
            {
                while let Ok(snapshot) = subscription.recv_shared().await {
                    listener.post(serde_json::to_value(subscription.latest_shared(snapshot)).unwrap());
                    next_frame().await;
                }
            }
        })
    }
    /// Acknowledge this exact completion locally, without changing the agent.
    pub async fn mark_seen(
        host_id: String,
        kind: String,
        session_id: String,
        completion: String,
    ) -> Result<(), String> {
        service::blocking(move || {
            observe(&service::process_home()?)?.seen(&host_id, &kind, &session_id, &completion)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riwork_engine::host_sessions::Session;
    #[test]
    fn slow_remote_hosts_do_not_starve_local_or_new_hosts() {
        let now = Instant::now();
        let mut cache = Cache::default();
        for key in ["local", "a", "b", "c", "d"] {
            cache.hosts.insert(
                key.into(),
                Host::pending(Some(key.into()), key.into(), "relay"),
            );
        }
        let mut state = State {
            cache,
            running: BTreeSet::from(["a".into(), "b".into(), "c".into()]),
            due: BTreeMap::new(),
            failures: BTreeMap::new(),
            registry_error: None,
            revision: 1,
            saved: 0,
            pins: BTreeMap::new(),
            forgets: 0,
        };
        assert_eq!(due_keys(&state, now), vec!["local"]);
        state.running.remove("a");
        state.due.insert("a".into(), now);
        assert_eq!(due_keys(&state, now), vec!["local", "d"]);
    }
    #[tokio::test]
    async fn cached_host_survives_failure_restart_and_changed_identity() {
        let home = tempfile::tempdir().unwrap();
        let snapshot = LocalSnapshot {
            version: 1,
            host_id: uuid::Uuid::new_v4().to_string(),
            observed_at: 5,
            capabilities: vec!["chats".into()],
            total: 1,
            partial: false,
            sessions: vec![Session {
                id: "chat".into(),
                kind: "chat".into(),
                project_id: None,
                parent_id: None,
                title: "Agent".into(),
                provider: None,
                state: "working".into(),
                source: "provider".into(),
                since: Some(4),
                subagents: None,
                requests: vec![],
                completion: None,
                unseen: false,
            }],
        };
        let mut cache = Cache::default();
        cache.hosts.insert(
            "route".into(),
            Host::pending(Some("route".into()), "Remote".into(), "relay"),
        );
        let state = State {
            cache,
            running: BTreeSet::new(),
            due: BTreeMap::new(),
            failures: BTreeMap::new(),
            registry_error: None,
            revision: 1,
            saved: 0,
            pins: BTreeMap::new(),
            forgets: 0,
        };
        let service = HostsService {
            home: home.path().into(),
            topic: Hub::new(HostsService::snapshot_locked(&state)),
            state: Mutex::new(state),
            requested: AtomicBool::new(false),
            registry_running: AtomicBool::new(false),
            persistence: Mutex::new(()),
        };
        let subscription = service.subscribe();
        let _ = subscription.recv().await.unwrap();
        service.complete("route", Ok(snapshot.clone()));
        let live = subscription.recv().await.unwrap();
        assert_eq!(live.data["hosts"][0]["freshness"], "current");
        service.complete("route", Err(ReadFailure::Backoff("connection lost".into())));
        let offline = subscription.recv().await.unwrap();
        assert!(offline.rev > live.rev);
        assert_eq!(
            offline.data["hosts"][0]["snapshot"]["sessions"][0]["state"],
            "working"
        );
        service.persist().unwrap();
        let restored = Cache::open(home.path());
        assert_eq!(restored.hosts["route"].freshness, "stale");
        assert_eq!(
            restored.hosts["route"].snapshot.as_ref().unwrap().host_id,
            snapshot.host_id
        );
        let mut different = snapshot;
        different.host_id = uuid::Uuid::new_v4().to_string();
        service.complete("route", Ok(different));
        let changed = subscription.recv().await.unwrap();
        assert_eq!(changed.data["hosts"][0]["connection"], "blocked");
        assert_eq!(changed.data["hosts"][0]["id"], restored.hosts["route"].id);
    }
    fn service_with(home: &Path, routes: &[&str]) -> HostsService {
        let mut cache = Cache::default();
        for route in routes {
            let connection = (*route != "local").then(|| (*route).to_owned());
            cache.hosts.insert(
                (*route).into(),
                Host::pending(connection, (*route).into(), "relay"),
            );
        }
        let state = State {
            cache,
            running: BTreeSet::new(),
            due: BTreeMap::new(),
            failures: BTreeMap::new(),
            registry_error: None,
            revision: 1,
            saved: 0,
            pins: BTreeMap::new(),
            forgets: 0,
        };
        HostsService {
            home: home.into(),
            topic: Hub::new(HostsService::snapshot_locked(&state)),
            state: Mutex::new(state),
            requested: AtomicBool::new(false),
            registry_running: AtomicBool::new(false),
            persistence: Mutex::new(()),
        }
    }
    fn read_with(
        host_id: &str,
        observed_at: u64,
        rows: &[(&str, Option<u64>, Option<&str>)],
    ) -> LocalSnapshot {
        LocalSnapshot {
            version: 1,
            host_id: host_id.into(),
            observed_at,
            capabilities: vec!["chats".into()],
            total: rows.len(),
            partial: false,
            sessions: rows
                .iter()
                .map(|(id, since, completion)| Session {
                    id: (*id).into(),
                    kind: "chat".into(),
                    project_id: None,
                    parent_id: None,
                    title: "Agent".into(),
                    provider: None,
                    state: "idle".into(),
                    source: "provider".into(),
                    since: *since,
                    subagents: None,
                    requests: vec![],
                    completion: completion.map(str::to_owned),
                    unseen: false,
                })
                .collect(),
        }
    }
    fn new_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }
    fn row_of<'a>(value: &'a Value, connection: Option<&str>) -> &'a Value {
        value["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|host| host["connectionId"].as_str() == connection)
            .unwrap_or_else(|| panic!("no row for {connection:?} in {value}"))
    }

    // C1: what a failed read says about the route.
    #[test]
    fn a_failed_read_is_classified_by_what_it_says_about_the_route() {
        const UPDATE: &str = "This host needs a Somtus update to report agent sessions";
        let rpc = |code: &str, message: &str| RemoteError::Rpc {
            code: code.into(),
            message: message.into(),
        };
        // All three shapes an older host refuses Hosts.local in, whose raw text helps nobody.
        for message in [
            "unsupported RPC method",
            "Unsupported RPC method: native.invoke",
            "unsupported native method",
        ] {
            assert_eq!(
                classify(&rpc("invalid_request", message)),
                ReadFailure::Blocked(UPDATE.into()),
                "{message}"
            );
        }
        assert_eq!(
            classify(&RemoteError::NeedsSetup {
                cause: "identity_changed".into(),
                message: "Remove and add this host".into()
            }),
            ReadFailure::Blocked("Remove and add this host".into())
        );
        assert_eq!(
            classify(&rpc("response_too_large", "Too large to show on the phone")),
            ReadFailure::Blocked("Too large to show on the phone".into())
        );
        // The link answered; the host could not report.
        assert_eq!(
            classify(&rpc("cli_error", "tmux timed out")),
            ReadFailure::HostError("tmux timed out".into())
        );
        // Everything else is link trouble, including the same code for another reason and the
        // words the old list expected, which nothing sends.
        assert_eq!(
            classify(&rpc("invalid_request", "params must be an object")),
            ReadFailure::Backoff("params must be an object".into())
        );
        for code in [
            "offline",
            "unsupported",
            "forbidden",
            "unauthorized",
            "invalid",
            "method_not_found",
            "unsupported_method",
            "permission_denied",
        ] {
            assert_eq!(
                classify(&rpc(code, "words")),
                ReadFailure::Backoff("words".into()),
                "{code}"
            );
        }
        assert!(matches!(
            classify(&RemoteError::Timeout),
            ReadFailure::Backoff(_)
        ));
        assert!(matches!(
            classify(&RemoteError::Lost("gone".into())),
            ReadFailure::Backoff(_)
        ));
        assert!(matches!(
            classify(&RemoteError::Unreachable("no daemon".into())),
            ReadFailure::Backoff(_)
        ));
        assert!(matches!(
            classify(&RemoteError::Protocol("garbled".into())),
            ReadFailure::Backoff(_)
        ));
    }
    #[tokio::test]
    async fn each_kind_of_failure_reaches_the_row_as_itself() {
        let home = tempfile::tempdir().unwrap();
        let service = service_with(home.path(), &["local", "route"]);
        let subscription = service.subscribe();
        let _ = subscription.recv().await.unwrap();
        let id = new_id();
        service.complete("route", Ok(read_with(&id, 5, &[("chat", None, None)])));
        let _ = subscription.recv().await.unwrap();

        // The link is fine, so the row must not read as disconnected.
        service.complete(
            "route",
            Err(ReadFailure::HostError("tmux timed out".into())),
        );
        let snapshot = subscription.recv().await.unwrap().data;
        let row = row_of(&snapshot, Some("route"));
        assert_eq!(row["connection"], "online");
        assert_eq!(row["freshness"], "stale");
        assert_eq!(row["error"], "tmux timed out");
        assert_eq!(row["snapshot"]["hostId"], id.as_str());

        service.complete("route", Err(ReadFailure::Blocked("Needs an update".into())));
        let snapshot = subscription.recv().await.unwrap().data;
        let row = row_of(&snapshot, Some("route"));
        assert_eq!(row["connection"], "blocked");
        assert_eq!(row["error"], "Needs an update");

        service.complete("route", Err(ReadFailure::Backoff("relay closed".into())));
        let snapshot = subscription.recv().await.unwrap().data;
        assert_eq!(row_of(&snapshot, Some("route"))["connection"], "backoff");

        // This machine has no link to lose, and failures keep its row's last known state.
        service.complete(
            "local",
            Err(ReadFailure::HostError("tmux timed out".into())),
        );
        let snapshot = subscription.recv().await.unwrap().data;
        let row = row_of(&snapshot, None);
        assert_eq!(row["connection"], "online");
        assert_eq!(row["freshness"], "unknown");
        assert_eq!(row["error"], "tmux timed out");
    }

    // C4 and #42: the link is asked first.
    fn link(
        state: LinkState,
        reason: Option<&str>,
        needs_action: bool,
    ) -> Result<HostStatus, String> {
        Ok(HostStatus {
            state,
            rtt_ms: None,
            since: None,
            reason: reason.map(str::to_owned),
            label: None,
            cause: needs_action.then(|| "identity_changed".to_owned()),
            needs_action,
        })
    }
    #[test]
    fn the_read_goes_ahead_only_on_a_link_that_is_up() {
        let id = new_id();
        let refuse_call = || -> Result<Value, RemoteError> { panic!("the host was called") };
        assert_eq!(
            read_remote(link(LinkState::Connecting, None, false), refuse_call),
            Err(ReadFailure::Connecting)
        );
        assert_eq!(
            read_remote(
                link(LinkState::Offline, Some("relay closed"), false),
                refuse_call
            ),
            Err(ReadFailure::Backoff("relay closed".into()))
        );
        assert_eq!(
            read_remote(
                link(LinkState::Offline, Some("Remove and add it"), true),
                refuse_call
            ),
            Err(ReadFailure::Blocked("Remove and add it".into()))
        );
        assert!(matches!(
            read_remote(link(LinkState::Offline, None, false), refuse_call),
            Err(ReadFailure::Backoff(reason)) if !reason.is_empty()
        ));
        assert_eq!(
            read_remote(Err("Cannot start the connection".into()), refuse_call),
            Err(ReadFailure::Backoff("Cannot start the connection".into()))
        );
        let value = serde_json::to_value(read_with(&id, 5, &[])).unwrap();
        let read = read_remote(link(LinkState::Online, None, false), || {
            Ok(json!({ "value": value }))
        })
        .unwrap();
        assert_eq!(read.host_id, id);
        // The call's own answer still decides once the link was up.
        assert_eq!(
            read_remote(link(LinkState::Online, None, false), || {
                Err(RemoteError::Rpc {
                    code: "invalid_request".into(),
                    message: "unsupported RPC method".into(),
                })
            }),
            Err(ReadFailure::Blocked(
                "This host needs a Somtus update to report agent sessions".into()
            ))
        );
        assert_eq!(
            read_remote(link(LinkState::Online, None, false), || {
                Ok(json!({ "value": { "version": "not a snapshot" } }))
            }),
            Err(ReadFailure::Blocked(
                "This host needs a Somtus update to report agent sessions".into()
            ))
        );
    }
    #[test]
    fn a_connecting_link_is_due_soon_and_counts_no_failure() {
        let home = tempfile::tempdir().unwrap();
        let service = service_with(home.path(), &["route"]);
        let id = new_id();
        service.complete("route", Ok(read_with(&id, 5, &[("chat", None, None)])));
        let started = Instant::now();
        service.complete("route", Err(ReadFailure::Connecting));
        {
            let state = service.state.lock().unwrap();
            assert_eq!(state.failures["route"], 0);
            let host = &state.cache.hosts["route"];
            assert_eq!(host.connection, "connecting");
            assert_eq!(host.freshness, "stale");
            assert!(host.snapshot.is_some());
            let due = state.due["route"];
            assert!(due >= started + Duration::from_millis(500), "too soon");
            assert!(
                due <= Instant::now() + Duration::from_millis(1500),
                "too late"
            );
        }
        // Failures so far stay as they were: it says nothing either way.
        service.complete("route", Err(ReadFailure::Backoff("relay closed".into())));
        service.complete("route", Err(ReadFailure::Connecting));
        assert_eq!(service.state.lock().unwrap().failures["route"], 1);
        service.complete("route", Err(ReadFailure::Blocked("Needs an update".into())));
        service.complete("route", Err(ReadFailure::Connecting));
        let state = service.state.lock().unwrap();
        assert_eq!(state.failures["route"], 2);
        assert_eq!(state.cache.hosts["route"].connection, "blocked");
    }
    #[test]
    fn hosts_with_failures_leave_a_remote_slot_for_healthy_ones() {
        let now = Instant::now();
        let mut cache = Cache::default();
        for key in ["a", "b", "c", "d", "e"] {
            cache.hosts.insert(
                key.into(),
                Host::pending(Some(key.into()), key.into(), "relay"),
            );
        }
        let mut state = State {
            cache,
            running: BTreeSet::from(["a".into(), "b".into()]),
            due: BTreeMap::new(),
            failures: BTreeMap::from([
                ("a".into(), 1),
                ("b".into(), 3),
                ("c".into(), 1),
                ("e".into(), 0),
            ]),
            registry_error: None,
            revision: 1,
            saved: 0,
            pins: BTreeMap::new(),
            forgets: 0,
        };
        // Two failing hosts are running, so the third slot goes to a healthy host even though
        // the failing one is earlier in line.
        assert_eq!(due_keys(&state, now), vec!["d"]);
        // A failing host never takes the last slot, even with nobody else waiting.
        state.due.insert("d".into(), now + Duration::from_secs(5));
        state.due.insert("e".into(), now + Duration::from_secs(5));
        assert_eq!(due_keys(&state, now), Vec::<String>::new());
        // With one failing read running, one more may start, and healthy hosts take the rest.
        state.running = BTreeSet::from(["a".into()]);
        state.due.insert("b".into(), now);
        state.due.insert("c".into(), now);
        state.due.insert("d".into(), now);
        state.due.insert("e".into(), now);
        assert_eq!(due_keys(&state, now), vec!["b", "d"]);
        // This machine's own read counts against neither limit, however often it failed.
        state.cache.hosts.insert(
            "local".into(),
            Host::pending(None, "This Mac".into(), "local"),
        );
        state.failures.insert("local".into(), 5);
        assert_eq!(due_keys(&state, now), vec!["local", "b", "d"]);
        state.running = BTreeSet::from(["local".into(), "a".into()]);
        assert_eq!(due_keys(&state, now), vec!["b", "d"]);
        // Nothing failing: all three slots are open as before.
        state.failures.clear();
        state.running.clear();
        assert_eq!(due_keys(&state, now), vec!["local", "a", "b", "c"]);
    }

    // C3: replacing a route is what clears its pin.
    #[test]
    fn a_route_added_again_is_a_new_record_with_no_pin() {
        let home = tempfile::tempdir().unwrap();
        let service = service_with(home.path(), &[]);
        let route = |added_at| RegisteredHost {
            host: riwork_engine::remote_hosts::Host {
                id: "route".into(),
                label: "Studio".into(),
            },
            transport: "relay".into(),
            added_at,
            host_id: None,
        };
        let registry = |routes| {
            apply_registry(&mut service.state.lock().unwrap(), routes);
        };
        registry(vec![route(Some(100))]);
        let (first, second) = (new_id(), new_id());
        service.complete("route", Ok(read_with(&first, 5, &[("chat", None, None)])));
        {
            let state = service.state.lock().unwrap();
            assert_eq!(state.cache.hosts["route"].added_at, Some(100));
            assert_eq!(state.cache.hosts["route"].id, first);
        }
        // The same record answering as another host stays refused, whatever the registry says.
        registry(vec![route(Some(100))]);
        service.complete("route", Ok(read_with(&second, 6, &[])));
        {
            let state = service.state.lock().unwrap();
            let host = &state.cache.hosts["route"];
            assert_eq!(host.connection, "blocked");
            assert_eq!(host.id, first);
        }
        // Removed and paired again: same id, newer record. The refusal's own advice now works.
        registry(vec![route(Some(200))]);
        {
            let mut state = service.state.lock().unwrap();
            let host = &state.cache.hosts["route"];
            assert_eq!(host.connection, "connecting");
            assert_eq!(host.freshness, "unknown");
            assert_eq!(host.snapshot, None);
            assert_eq!(host.received_at, None);
            assert_eq!(host.error, None);
            assert_eq!(host.added_at, Some(200));
            assert_eq!(host.label, "Studio");
            assert!(!state.failures.contains_key("route"));
            assert!(!state.due.contains_key("route"));
            state.failures.clear();
        }
        service.complete("route", Ok(read_with(&second, 7, &[])));
        {
            let state = service.state.lock().unwrap();
            let host = &state.cache.hosts["route"];
            assert_eq!(host.connection, "online");
            assert_eq!(host.id, second);
        }
        // A pin is only cleared on evidence: either side not knowing the time is not evidence.
        registry(vec![route(None)]);
        service.complete("route", Ok(read_with(&first, 8, &[])));
        assert_eq!(
            service.state.lock().unwrap().cache.hosts["route"].connection,
            "blocked"
        );
        assert_eq!(
            service.state.lock().unwrap().cache.hosts["route"].added_at,
            Some(200)
        );
        service
            .state
            .lock()
            .unwrap()
            .cache
            .hosts
            .get_mut("route")
            .unwrap()
            .added_at = None;
        registry(vec![route(Some(300))]);
        {
            let state = service.state.lock().unwrap();
            let host = &state.cache.hosts["route"];
            assert_eq!(host.id, second, "a row from an older cache keeps its pin");
            assert_eq!(host.added_at, Some(300));
        }
        // The time survives a restart, so a re-pair with the app closed is noticed too.
        service
            .state
            .lock()
            .unwrap()
            .cache
            .hosts
            .get_mut("route")
            .unwrap()
            .connection = "online".into();
        service.state.lock().unwrap().revision += 1;
        service.persist().unwrap();
        assert_eq!(Cache::open(home.path()).hosts["route"].added_at, Some(300));
        // A registry row that went away is still simply dropped, along with its bookkeeping.
        service
            .state
            .lock()
            .unwrap()
            .failures
            .insert("route".into(), 2);
        registry(vec![]);
        let state = service.state.lock().unwrap();
        assert!(!state.cache.hosts.contains_key("route"));
        assert!(!state.failures.contains_key("route"));
    }
    // #7: the dial checks the identity a host announces; the inventory names one of its own.
    #[test]
    fn an_ssh_route_is_read_only_as_the_host_it_was_added_for() {
        let home = tempfile::tempdir().unwrap();
        let service = service_with(home.path(), &["ssh"]);
        let (pinned, other) = (new_id(), new_id());
        let route = |kind: &str, pin: Option<&str>| RegisteredHost {
            host: riwork_engine::remote_hosts::Host {
                id: "ssh".into(),
                label: "Box".into(),
            },
            transport: kind.into(),
            added_at: Some(100),
            host_id: pin.map(str::to_owned),
        };
        let theirs = [("theirs", Some(1), Some("turn:1:completed"))];
        // A read that finished before the registry was known, kept from before.
        service.complete("ssh", Ok(read_with(&other, u64::MAX, &theirs)));
        apply_registry(
            &mut service.state.lock().unwrap(),
            vec![route("ssh", Some(&pinned))],
        );
        let snapshot = service.snapshot();
        assert!(!snapshot.to_string().contains("theirs"), "{snapshot}");
        // And every answer for another identity after that, this Mac's for instance.
        service.complete("ssh", Ok(read_with(&other, u64::MAX, &theirs)));
        let snapshot = service.snapshot();
        let row = row_of(&snapshot, Some("ssh"));
        assert_eq!(row["connection"], "blocked");
        assert_eq!(row["error"], NOT_PINNED_HOST);
        assert_eq!(row["snapshot"], Value::Null);
        assert!(!snapshot.to_string().contains("theirs"), "{snapshot}");
        // The host it was added for reads as usual.
        service.complete("ssh", Ok(read_with(&pinned, 5, &[("mine", None, None)])));
        let snapshot = service.snapshot();
        let row = row_of(&snapshot, Some("ssh"));
        assert_eq!(row["connection"], "online");
        assert_eq!(row["snapshot"]["sessions"][0]["id"], "mine");
        // A relay route has no pin: what it read first is what holds it.
        let relay = service_with(home.path(), &["ssh"]);
        apply_registry(&mut relay.state.lock().unwrap(), vec![route("relay", None)]);
        relay.complete("ssh", Ok(read_with(&other, 5, &[])));
        assert_eq!(
            row_of(&relay.snapshot(), Some("ssh"))["connection"],
            "online"
        );
    }
    // #52: the list runs outside the lock, and a removal can finish meanwhile.
    #[test]
    fn a_registry_list_from_before_a_removal_does_not_bring_the_route_back() {
        let home = tempfile::tempdir().unwrap();
        let service = service_with(home.path(), &["local", "route"]);
        let listed = |id: &str| RegisteredHost {
            host: riwork_engine::remote_hosts::Host {
                id: id.into(),
                label: id.into(),
            },
            transport: "relay".into(),
            added_at: Some(100),
            host_id: None,
        };
        service.refresh_registry_with(|| {
            service.forget("route").unwrap();
            Ok(vec![listed("route"), listed("other")])
        });
        assert!(
            !service
                .state
                .lock()
                .unwrap()
                .cache
                .hosts
                .contains_key("route")
        );
        // A list read after the removal is applied as usual.
        service.refresh_registry_with(|| Ok(vec![listed("other")]));
        let state = service.state.lock().unwrap();
        assert!(state.cache.hosts.contains_key("other"));
        assert!(!state.cache.hosts.contains_key("route"));
    }
    // #22: a caller can ask without subscribing, as a mod polling hosts.get does.
    #[test]
    fn a_caller_polling_without_subscribing_sees_a_down_host_as_down() {
        let home = tempfile::tempdir().unwrap();
        let service = service_with(home.path(), &["route"]);
        service.complete(
            "route",
            Ok(read_with(&new_id(), 5, &[("chat", None, None)])),
        );
        service.complete("route", Err(ReadFailure::Backoff("relay closed".into())));
        let long_ago = riwork_engine::activity::unix_now() - 3600;
        service
            .state
            .lock()
            .unwrap()
            .cache
            .hosts
            .get_mut("route")
            .unwrap()
            .received_at = Some(long_ago);
        for _ in 0..3 {
            let snapshot = service.snapshot();
            let row = row_of(&snapshot, Some("route"));
            assert_eq!(row["connection"], "backoff");
            assert_eq!(row["error"], "relay closed");
        }
    }
    #[test]
    fn this_machine_recovers_from_a_new_identity_by_itself() {
        let home = tempfile::tempdir().unwrap();
        let service = service_with(home.path(), &["local"]);
        let (first, second) = (new_id(), new_id());
        service.complete("local", Ok(read_with(&first, 5, &[])));
        service.complete("local", Ok(read_with(&second, 6, &[])));
        let state = service.state.lock().unwrap();
        let host = &state.cache.hosts["local"];
        assert_eq!(host.connection, "online");
        assert_eq!(host.id, second);
        assert_eq!(host.error, None);
    }

    // C5: a view that returns must not be told about the past as if it were now.
    #[tokio::test]
    async fn a_returning_observer_never_sees_old_rows_as_current() {
        let home = tempfile::tempdir().unwrap();
        let service = service_with(home.path(), &["old", "blocked", "recent"]);
        let (old, blocked, recent) = (new_id(), new_id(), new_id());
        let working = [("chat", None, None)];
        service.complete("old", Ok(read_with(&old, 5, &working)));
        service.complete("blocked", Ok(read_with(&blocked, 5, &working)));
        service.complete(
            "blocked",
            Err(ReadFailure::Blocked("Needs an update".into())),
        );
        service.complete("recent", Ok(read_with(&recent, 5, &working)));
        let long_ago = riwork_engine::activity::unix_now() - 3600;
        {
            let mut state = service.state.lock().unwrap();
            state.cache.hosts.get_mut("old").unwrap().received_at = Some(long_ago);
            state.cache.hosts.get_mut("blocked").unwrap().received_at = Some(long_ago);
        }
        let subscription = service.subscribe();
        let first = subscription.recv().await.unwrap().data;
        let row = row_of(&first, Some("old"));
        assert_eq!(row["connection"], "connecting");
        assert_eq!(row["freshness"], "stale");
        assert_eq!(row["receivedAt"], long_ago);
        assert_eq!(row["snapshot"]["hostId"], old.as_str());
        assert_eq!(row["snapshot"]["sessions"][0]["state"], "idle");
        assert_eq!(row_of(&first, Some("blocked"))["connection"], "blocked");
        assert_eq!(row_of(&first, Some("recent"))["connection"], "online");

        // While someone watches, the reads themselves keep rows honest: no aging.
        service.complete("old", Ok(read_with(&old, 6, &working)));
        {
            let mut state = service.state.lock().unwrap();
            state.cache.hosts.get_mut("old").unwrap().received_at = Some(long_ago);
        }
        let second = service.subscribe();
        let _ = subscription.recv().await.unwrap();
        let delivered = second.recv().await.unwrap().data;
        assert_eq!(row_of(&delivered, Some("old"))["connection"], "online");
        assert_eq!(
            row_of(&service.snapshot(), Some("old"))["connection"],
            "online"
        );
        drop(subscription);
        drop(second);

        // A one-shot read after everyone left gets the same honesty.
        assert_eq!(
            row_of(&service.snapshot(), Some("old"))["connection"],
            "connecting"
        );
        assert_eq!(
            row_of(&service.snapshot(), Some("old"))["freshness"],
            "stale"
        );
    }

    // C7: first contact, through the path every real read takes.
    #[test]
    fn the_read_path_baselines_history_and_keeps_later_completions() {
        let home = tempfile::tempdir().unwrap();
        let service = service_with(home.path(), &["route"]);
        let id = new_id();
        let now = riwork_engine::activity::unix_now();
        let unseen = |value: &Value, session: &str| {
            row_of(value, Some("route"))["snapshot"]["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == session)
                .unwrap()["unseen"]
                .clone()
        };
        let history = ("history", Some(now - 1000), Some("turn:1:completed"));
        service.complete("route", Ok(read_with(&id, now, &[history])));
        assert_eq!(unseen(&service.snapshot(), "history"), false);
        let next = ("next", Some(now + 1), Some("turn:2:completed"));
        service.complete("route", Ok(read_with(&id, now + 5, &[history, next])));
        assert_eq!(unseen(&service.snapshot(), "history"), false);
        assert_eq!(unseen(&service.snapshot(), "next"), true);
        service.persist().unwrap();
        let restored = Cache::open(home.path());
        let rows = restored.rows();
        let sessions = &rows[0].snapshot.as_ref().unwrap().sessions;
        assert!(
            !sessions
                .iter()
                .find(|row| row.id == "history")
                .unwrap()
                .unseen
        );
        assert!(sessions.iter().find(|row| row.id == "next").unwrap().unseen);
        // A completion is acknowledged one row at a time as before.
        service
            .seen(&id, "chat", "next", "turn:2:completed")
            .unwrap();
        assert_eq!(unseen(&service.snapshot(), "next"), false);
    }

    // #44: removing a route takes effect at once.
    #[test]
    fn a_forgotten_route_is_gone_from_views_and_disk_at_once() {
        let home = tempfile::tempdir().unwrap();
        let service = Arc::new(service_with(home.path(), &["local", "route", "other"]));
        let id = new_id();
        service.complete(
            "route",
            Ok(read_with(&id, 5, &[("secret title", None, None)])),
        );
        service.complete("other", Ok(read_with(&new_id(), 5, &[])));
        service.persist().unwrap();
        assert!(Cache::open(home.path()).hosts.contains_key("route"));
        SERVICES
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(home.path().into(), service.clone());
        // A read in flight when the route goes must only clear its bookkeeping.
        service.state.lock().unwrap().running.insert("route".into());
        service
            .state
            .lock()
            .unwrap()
            .failures
            .insert("route".into(), 3);

        forget_route(home.path(), "route").unwrap();

        let snapshot = service.snapshot();
        assert_eq!(snapshot["hosts"].as_array().unwrap().len(), 2, "{snapshot}");
        assert!(!snapshot.to_string().contains("secret title"));
        let saved = Cache::open(home.path());
        assert!(!saved.hosts.contains_key("route"));
        assert!(saved.hosts.contains_key("other"));
        assert!(
            !std::fs::read_to_string(home.path().join("host-sessions.json"))
                .unwrap()
                .contains("secret title")
        );
        {
            let state = service.state.lock().unwrap();
            assert!(!state.due.contains_key("route"));
            assert!(!state.failures.contains_key("route"));
            assert_eq!(
                due_keys(&state, Instant::now())
                    .iter()
                    .filter(|key| *key == "route")
                    .count(),
                0
            );
        }
        service.complete(
            "route",
            Ok(read_with(&id, 6, &[("secret title", None, None)])),
        );
        let state = service.state.lock().unwrap();
        assert!(!state.running.contains("route"));
        assert!(!state.cache.hosts.contains_key("route"));
        drop(state);
        // This machine's row is not a route, and an unknown id changes nothing.
        forget_route(home.path(), "local").unwrap();
        forget_route(home.path(), "never-there").unwrap();
        assert_eq!(service.snapshot()["hosts"].as_array().unwrap().len(), 2);
        SERVICES.get().unwrap().lock().unwrap().remove(home.path());
    }
    #[test]
    fn forgetting_a_route_with_nothing_watching_still_clears_the_file() {
        let home = tempfile::tempdir().unwrap();
        let mut cache = Cache::default();
        for route in ["route", "other"] {
            cache.hosts.insert(
                route.into(),
                Host::pending(Some(route.into()), route.into(), "relay"),
            );
            cache
                .apply_read(
                    route,
                    read_with(&new_id(), 5, &[("secret title", None, None)]),
                    5,
                )
                .unwrap();
        }
        cache.save(home.path()).unwrap();
        forget_route(home.path(), "route").unwrap();
        let saved = Cache::open(home.path());
        assert!(!saved.hosts.contains_key("route"));
        assert!(saved.hosts.contains_key("other"));
        // No file, or nothing to remove: nothing to do, and no file made.
        let empty = tempfile::tempdir().unwrap();
        forget_route(empty.path(), "route").unwrap();
        assert!(!empty.path().join("host-sessions.json").exists());
    }
    #[test]
    fn failed_persistence_keeps_live_inventory_and_dirty_revision() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("host-sessions.json");
        std::fs::create_dir(&path).unwrap();
        let mut cache = Cache::default();
        cache.hosts.insert(
            "pending".into(),
            Host::pending(Some("pending".into()), "Not read yet".into(), "relay"),
        );
        let original = serde_json::to_vec(&cache).unwrap();
        let state = State {
            cache,
            running: BTreeSet::new(),
            due: BTreeMap::new(),
            failures: BTreeMap::new(),
            registry_error: None,
            revision: 7,
            saved: 2,
            pins: BTreeMap::new(),
            forgets: 0,
        };
        let service = HostsService {
            home: home.path().into(),
            topic: Hub::new(HostsService::snapshot_locked(&state)),
            state: Mutex::new(state),
            requested: AtomicBool::new(false),
            registry_running: AtomicBool::new(false),
            persistence: Mutex::new(()),
        };
        assert!(service.persist().is_err());
        {
            let state = service.state.lock().unwrap();
            assert_eq!(state.saved, 2);
            assert_eq!(state.revision, 7);
            assert_eq!(serde_json::to_vec(&state.cache).unwrap(), original);
        }
        std::fs::remove_dir(&path).unwrap();
        service.persist().unwrap();
        {
            let state = service.state.lock().unwrap();
            assert_eq!(state.saved, 7);
            assert_eq!(serde_json::to_vec(&state.cache).unwrap(), original);
        }
        assert!(Cache::open(home.path()).hosts.is_empty());
        // Once this generation is saved, no new write is attempted.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        service.persist().unwrap();
    }
}
