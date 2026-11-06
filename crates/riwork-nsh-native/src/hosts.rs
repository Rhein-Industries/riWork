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
    remote_hosts::{RemoteCli, RemoteError},
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

struct State {
    cache: Cache,
    running: BTreeSet<String>,
    due: BTreeMap<String, Instant>,
    failures: BTreeMap<String, u32>,
    registry_error: Option<String>,
    revision: u64,
    saved: u64,
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
        Self::snapshot_locked(&self.state.lock().unwrap())
    }
    pub fn subscribe(&self) -> TopicSubscription<Value> {
        self.requested.store(true, Ordering::Release);
        self.topic.subscribe()
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
    fn complete(&self, key: &str, result: Result<LocalSnapshot, (String, bool)>) {
        let mut state = self.state.lock().unwrap();
        state.running.remove(key);
        let Some(host) = state.cache.hosts.get_mut(key) else {
            return;
        };
        let result = result.and_then(|snapshot| {
            host.apply(snapshot, riwork_engine::activity::unix_now())
                .map_err(|e| (e, true))
        });
        let ok = result.is_ok();
        if let Err((message, blocked)) = result {
            host.failed(message, blocked);
        }
        let failed = state.failures.entry(key.to_owned()).or_default();
        *failed = if ok { 0 } else { (*failed + 1).min(5) };
        let delay = if key == "local" {
            2
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
        let hosts: Result<Vec<(riwork_engine::remote_hosts::Host, String)>, String> = Ok(vec![]);
        #[cfg(not(feature = "test-support"))]
        let hosts = RemoteCli::locate().and_then(|cli| cli.hosts_with_transport());
        let mut state = self.state.lock().unwrap();
        match hosts {
            Err(e) => state.registry_error = Some(e),
            Ok(hosts) => {
                state.registry_error = (hosts.len() > model::MAX_HOSTS)
                    .then(|| format!("Showing the first {} hosts", model::MAX_HOSTS));
                let ids: BTreeSet<_> = hosts
                    .iter()
                    .take(model::MAX_HOSTS)
                    .map(|(h, _)| h.id.clone())
                    .chain(["local".into()])
                    .collect();
                state.cache.hosts.retain(|id, _| ids.contains(id));
                state.due.retain(|id, _| ids.contains(id));
                for (host, transport) in hosts.into_iter().take(model::MAX_HOSTS) {
                    let row = state.cache.hosts.entry(host.id.clone()).or_insert_with(|| {
                        Host::pending(Some(host.id), host.label.clone(), &transport)
                    });
                    row.label = model::bounded(&host.label, 256);
                    row.route = transport;
                }
            }
        }
        state.revision += 1;
        self.publish(&state);
    }
}

// Reserve one slot for the local host. Slow/offline machines cannot starve it;
// never-read and oldest-due remote hosts win over another retry of an early key.
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
    due.retain(|key| {
        if key == "local" {
            true
        } else if remote_slots > 0 {
            remote_slots -= 1;
            true
        } else {
            false
        }
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
                                model::local(&service.home).map_err(|e| (e, false))
                            } else {
                                remote_snapshot(&key)
                            };
                            service.complete(&key, result);
                        })
                    {
                        copy.complete(&failed_key, Err((error.to_string(), false)));
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    services.insert(home.to_path_buf(), service.clone());
    Ok(service)
}

fn remote_snapshot(host: &str) -> Result<LocalSnapshot, (String, bool)> {
    let result = crate::remote::call_host(
        host,
        "native.invoke",
        json!({"module":"Hosts","method":"local","args":{}}),
        Duration::from_secs(20),
    );
    let result = result.map_err(|error| {
        let blocked = matches!(&error,RemoteError::Rpc{code,..} if matches!(code.as_str(),"unsupported"|"forbidden"|"unauthorized"|"invalid"|"method_not_found"|"unsupported_method"|"permission_denied"));
        (format!("{}", error.message()),blocked)
    })?;
    serde_json::from_value(result.get("value").cloned().unwrap_or(result)).map_err(|_| {
        (
            "This host needs a Somtus update to report agent sessions".into(),
            true,
        )
    })
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
        service.complete("route", Err(("connection lost".into(), false)));
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
