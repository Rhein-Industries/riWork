//! Phone access to explicitly registered SSH hosts through its paired desktop.
//! The first-hop session owns every downstream link. Nothing supplied by a
//! phone becomes an SSH address, command, credential or mutable route target.
use crate::{
    client::HostsFile,
    config::Storage,
    lanes::{self, Lane, Target},
    rpc::Request,
    ssh,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_SESSIONS: usize = 4;
/// How long before the phone's timer it gets its answer: the way back over the relay,
/// and what the clock on the phone had already counted when the request got here.
const ANSWER_MARGIN: Duration = Duration::from_secs(2);
/// The phone's timer for `hosts.open`.
const OPEN_TIMER: Duration = Duration::from_secs(30);
/// What a request has to have left to be started. With less it could only be answered by
/// its deadline, and what can be said then is worse than that it never went out.
const LEAST_TO_START: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(crate) struct SessionGateway {
    gateway: Option<Arc<Gateway>>,
    monitor: Option<tokio::task::JoinHandle<()>>,
}
impl SessionGateway {
    pub fn begin(&mut self, storage: Storage, device: String) {
        self.end();
        let gateway = Arc::new(Gateway::new(storage, device));
        let weak = Arc::downgrade(&gateway);
        let wake = gateway.wake.clone();
        self.monitor = Some(tokio::spawn(async move {
            loop {
                // Register before looking at the map so insertion cannot lose
                // its wake-up. Ordinary paired phones with no routed hosts do
                // no polling and incur no periodic idle wake-ups.
                let notified = wake.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let Some(gateway) = weak.upgrade() else {
                    return;
                };
                if gateway.closed.load(Ordering::Acquire) {
                    return;
                }
                let active = !gateway
                    .state
                    .lock()
                    .expect("gateway sessions")
                    .peers
                    .is_empty();
                if active {
                    gateway.prune_invalid();
                }
                drop(gateway);
                if active {
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {},
                        _ = &mut notified => {},
                    }
                } else {
                    notified.await;
                }
            }
        }));
        self.gateway = Some(gateway);
    }
    pub fn current(&self) -> Option<Arc<Gateway>> {
        self.gateway.clone()
    }
    pub fn end(&mut self) {
        if let Some(monitor) = self.monitor.take() {
            monitor.abort();
        }
        if let Some(gateway) = self.gateway.take() {
            gateway.close_all();
        }
    }
}
impl Drop for SessionGateway {
    fn drop(&mut self) {
        self.end();
    }
}

struct Peer {
    route_id: String,
    // The complete registry route as this session opened it, including paths and account.
    // `hosts update-ssh` can change the registry's copy; a session never follows it, and
    // ends when the two differ.
    route: ssh::Route,
    link: ssh::GatewayLink,
    features: Value,
}
struct State {
    peers: HashMap<String, Arc<Peer>>,
    opening: usize,
}
pub(crate) struct Gateway {
    storage: Storage,
    device: String,
    closed: AtomicBool,
    ended: tokio::sync::watch::Sender<bool>,
    wake: Arc<tokio::sync::Notify>,
    state: Mutex<State>,
}
struct Fault {
    code: String,
    message: String,
}
impl Fault {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    route_id: String,
    host_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Close {
    session: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    session: String,
    request_id: String,
    method: String,
    params: Value,
}
fn params<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, Fault> {
    serde_json::from_value(value.clone())
        .map_err(|_| Fault::new("invalid_request", "invalid host request parameters"))
}
fn uuid(value: &str) -> Result<(), Fault> {
    crate::crypto::uuid(value)
        .map(|_| ())
        .map_err(|_| Fault::new("invalid_request", "invalid UUID"))
}
fn closed() -> Fault {
    Fault::new(
        "host_session_closed",
        "the host session has ended; reconnect before continuing",
    )
}

/// How long the phone waits for the answer to a request before it gives up on its
/// whole connection, and with that on every route, the healthy ones too. Its
/// clock starts when it seals the request, so time in the queue here counts. A copy of
/// `RelayClient.timeout` (`ios/Core/RelayClient.swift`), kept in step by hand as
/// `rpc::native::METHODS` is; there too the inner method decides.
fn phone_timer(method: &str, params: &Value) -> Duration {
    /// `requestTimeout` there.
    const BASE: Duration = Duration::from_secs(15);
    /// `timeoutSlack` of `LiveSync` and `ChatLimits`.
    const WAIT_SLACK: Duration = Duration::from_secs(20);
    let wait = |longest_ms: f64| match params["wait_ms"].as_f64() {
        Some(ms) if ms > 0.0 => {
            BASE.max(Duration::from_millis(ms.min(longest_ms) as u64) + WAIT_SLACK)
        }
        _ => BASE,
    };
    match method {
        "shell.create" | "project.create" | "orchestrator.create" | "chat.create" => {
            Duration::from_secs(90)
        }
        "chat.command" => Duration::from_secs(60),
        "shell.close" | "chat.stop" | "upload.begin" | "upload.finish" | "shell.paste"
        | "project.memo.set" => Duration::from_secs(30),
        "chat.asset.read" if params["offset"].as_f64() == Some(0.0) => Duration::from_secs(90),
        "shell.output" => wait(10_000.0),
        // `RelayClient.timeout` has no line for `state.watch`, because no phone sends one
        // that waits yet. A wait that is cut short here would cost a healthy route, so
        // it gets what the other two waits get.
        "chat.events" | "state.watch" => wait(25_000.0),
        _ => BASE,
    }
}
/// When a request that goes to a host has to be answered, measured from when it was
/// `received`. Close to it the request is not started (`not_sent`), and one that has
/// started is answered without its reply. A request for this machine has no such time
/// once it runs: the limits of its CLI apply, as on a direct pairing.
pub(crate) fn deadline(target: &Target, request: &Value, received: Instant) -> Option<Instant> {
    let timer = match target {
        Target::Local => return None,
        Target::Route(_) => OPEN_TIMER,
        Target::Session(_) => {
            let inner = &request["params"];
            phone_timer(
                inner["method"].as_str().unwrap_or_default(),
                &inner["params"],
            )
        }
    };
    Some(received + timer - ANSWER_MARGIN)
}
/// When a request still waiting for its turn is answered as not sent. For one that goes
/// to a host that is when less than `LEAST_TO_START` is left of its `deadline`: started
/// later, it could only be refused, and its slot would go to what came after it. One for
/// this machine is on the same timer on the phone, and if that runs out the phone ends its
/// connection, and every route with it. The waits of a desktop and of a terminal stream's
/// lanes are not on such a timer.
pub(crate) fn queue_deadline(
    target: &Target,
    lane: Lane,
    desktop: bool,
    request: &Value,
    received: Instant,
) -> Option<Instant> {
    if *target != Target::Local {
        return deadline(target, request, received).map(|at| at - LEAST_TO_START);
    }
    if desktop || matches!(lane, Lane::Attach | Lane::Stream) {
        return None;
    }
    let method = request["method"].as_str().unwrap_or_default();
    Some(received + phone_timer(method, &request["params"]) - ANSWER_MARGIN)
}
fn not_sent_fault() -> Fault {
    Fault::new(
        "host_timeout",
        "this host is busy; the request was not sent",
    )
}
/// The answer to a request that was still waiting for its turn at its deadline.
pub(crate) fn not_sent(request: &Value) -> Value {
    let fault = not_sent_fault();
    let id = request.get("id").filter(|id| id.is_string());
    crate::rpc::error_for(
        id.cloned().unwrap_or(Value::Null),
        &fault.code,
        fault.message,
    )
}
fn too_late(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|at| at.saturating_duration_since(Instant::now()) < LEAST_TO_START)
}
/// Whether a forwarded request (the params of a `hosts.call`) cannot have changed anything
/// on its host, however it ended. The shared lane does not say so: the `upload.*` steps
/// write, and a `native.invoke` of a method this build has not heard of is there for want
/// of knowing better. The phone repeats what it was told did not happen, so anything not
/// known to be a read counts as a change.
fn known_read(inner: &Value) -> bool {
    let method = inner["method"].as_str().unwrap_or_default();
    if !allowed(method) || method.starts_with("upload.") {
        return false;
    }
    if method == "native.invoke" {
        let name = |key: &str| inner["params"][key].as_str().unwrap_or_default();
        return crate::rpc::native_is_write(name("module"), name("method")) == Some(false);
    }
    lanes::classify(inner) != Lane::Ordered
}
/// For a `select!` around a dial: resolves at `deadline`, if there is one.
async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}
/// What a phone is told about a dial that failed: a code it can act on and fixed words.
/// What the far side printed stays out of this, and so do the route's address and paths.
fn open_fault(error: &anyhow::Error) -> Fault {
    let code = ssh::dial_failure(error).map_or("host_unavailable", ssh::DialFailure::gateway_code);
    let message = match code {
        "host_changed" => {
            "this address now reaches another Somtus installation; if that is intended, remove the route and add it again on the gateway"
        }
        "host_busy" => {
            "an earlier session with this host is still finishing; try again in a moment"
        }
        "host_login_failed" => {
            "the gateway could not log in to this host over SSH; check the account and the host key on the gateway"
        }
        "host_unreachable" => {
            "the gateway cannot reach this host; check that it is awake and on the network"
        }
        "host_not_installed" => {
            "Somtus is missing or too old on this host; install or update it there"
        }
        "host_timeout" => {
            "this host did not finish connecting in time; check that SSH and Somtus work there"
        }
        "host_unsupported" => "this host must be updated to support forwarded sessions",
        _ => "cannot open this SSH host; check its connection and update Somtus on the host",
    };
    Fault::new(code, message)
}

/// Stable deduplication principal, independent of ephemeral connection/upload
/// tokens. Two phones (or two gateways with different registry client IDs)
/// never share a downstream input ledger.
fn client_identity(device: &str, route_id: &str, route: &ssh::Route) -> String {
    let mut hash = Sha256::new();
    hash.update(b"somtus.phone.gateway.client.v1\0");
    for value in [device, route_id, &route.client_id, &route.host_id] {
        hash.update(value.as_bytes());
        hash.update([0]);
    }
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

/// Deliberately finite. New first-hop/management methods are never forwarded
/// by accident; native.invoke retains the downstream native method allowlist.
pub(crate) fn allowed(method: &str) -> bool {
    matches!(
        method,
        "projects.list"
            | "worktrees.list"
            | "tasks.list"
            | "shells.list"
            | "orchestrators.list"
            | "shell.output"
            | "shell.history"
            | "shell.resize"
            | "shell.resize.clear"
            | "shell.input"
            | "appearance.get"
            | "dashboard.get"
            | "native.invoke"
            | "state.watch"
            | "shell.keys"
            | "shell.create"
            | "shell.close"
            | "project.create"
            | "project.memo.get"
            | "project.memo.set"
            | "orchestrator.create"
            | "tabs.list"
            | "tabs.update"
            | "tabs.open"
            | "chats.list"
            | "chat.create"
            | "chat.snapshot"
            | "chat.asset.read"
            | "chat.events"
            | "chat.command"
            | "chat.stop"
            | "chat.models"
            | "chat.suggestions"
            | "upload.begin"
            | "upload.chunk"
            | "upload.finish"
            | "upload.cancel"
            | "shell.paste"
    )
}

impl Gateway {
    fn new(storage: Storage, device: String) -> Self {
        Self {
            storage,
            device,
            closed: AtomicBool::new(false),
            ended: tokio::sync::watch::channel(false).0,
            wake: Arc::new(tokio::sync::Notify::new()),
            state: Mutex::new(State {
                peers: HashMap::new(),
                opening: 0,
            }),
        }
    }
    pub fn close_all(&self) {
        self.closed.store(true, Ordering::Release);
        self.ended.send_replace(true);
        self.wake.notify_waiters();
        let mut state = self.state.lock().expect("gateway sessions");
        for (_, peer) in state.peers.drain() {
            peer.link.close();
        }
    }
    fn prune_invalid(&self) {
        let peers: Vec<_> = self
            .state
            .lock()
            .expect("gateway sessions")
            .peers
            .iter()
            .map(|(token, peer)| (token.clone(), peer.clone()))
            .collect();
        if peers.is_empty() {
            return;
        }
        if !self.storage.authorized(&self.device).unwrap_or(false) {
            self.close_all();
            return;
        }
        let hosts = HostsFile::load(&self.storage);
        for (token, peer) in peers {
            let valid = peer.link.is_open()
                && hosts
                    .as_ref()
                    .ok()
                    .and_then(|file| file.find(&peer.route_id))
                    .and_then(|host| host.ssh.as_ref())
                    == Some(&peer.route);
            if !valid {
                peer.link.close();
                self.state
                    .lock()
                    .expect("gateway sessions")
                    .peers
                    .remove(&token);
            }
        }
    }
    fn check(&self, device: &str) -> Result<(), Fault> {
        if device != self.device || self.closed.load(Ordering::Acquire) {
            return Err(closed());
        }
        if !self.storage.authorized(device).unwrap_or(false) {
            return Err(closed());
        }
        Ok(())
    }
    fn route(&self, route_id: &str, host_id: &str) -> Result<ssh::Route, Fault> {
        let hosts = HostsFile::load(&self.storage)
            .map_err(|_| Fault::new("host_unavailable", "cannot read the registered hosts"))?;
        let route = hosts
            .find(route_id)
            .and_then(|host| host.ssh.clone())
            .ok_or_else(|| Fault::new("host_not_found", "this SSH host is no longer registered"))?;
        if route.host_id != host_id {
            return Err(Fault::new(
                "host_changed",
                "the registered host identity changed; select the host again",
            ));
        }
        Ok(route)
    }
    fn peer(&self, token: &str) -> Result<Arc<Peer>, Fault> {
        uuid(token)?;
        let peer = self
            .state
            .lock()
            .expect("gateway sessions")
            .peers
            .get(token)
            .cloned()
            .ok_or_else(closed)?;
        let current = self.route(&peer.route_id, &peer.route.host_id);
        if !current.as_ref().is_ok_and(|current| current == &peer.route) {
            peer.link.close();
            self.state
                .lock()
                .expect("gateway sessions")
                .peers
                .remove(token);
            return Err(current.err().unwrap_or_else(|| {
                Fault::new(
                    "host_changed",
                    "the registered SSH route changed; reopen the host",
                )
            }));
        }
        if !peer.link.is_open() {
            return Err(closed());
        }
        Ok(peer)
    }
    async fn dispatch(
        &self,
        device: &str,
        request: &Request,
        deadline: Option<Instant>,
    ) -> Result<Value, Fault> {
        self.check(device)?;
        match request.method.as_str() {
            "hosts.list" => {
                let _: Empty = params(&request.params)?;
                let file = HostsFile::load(&self.storage).map_err(|_| {
                    Fault::new("host_unavailable", "cannot read the registered hosts")
                })?;
                let hosts: Vec<_> = file.hosts.iter().filter_map(|host| host.ssh.as_ref().map(|route|
                    json!({"route_id":host.id,"host_id":route.host_id,"label":host.label,"transport":"ssh"})
                )).collect();
                Ok(json!({"v":1,"hosts":hosts}))
            }
            "hosts.open" => {
                let p: Open = params(&request.params)?;
                uuid(&p.route_id)?;
                uuid(&p.host_id)?;
                let route = self.route(&p.route_id, &p.host_id)?;
                self.prune_invalid();
                {
                    let mut state = self.state.lock().expect("gateway sessions");
                    if self.closed.load(Ordering::Acquire) {
                        return Err(closed());
                    }
                    if let Some((session, peer)) = state.peers.iter().find(|(_, peer)| {
                        peer.route_id == p.route_id && peer.route == route && peer.link.is_open()
                    }) {
                        return Ok(
                            json!({"session":session,"route_id":p.route_id,"host_id":p.host_id,"features":peer.features}),
                        );
                    }
                    if state.peers.len() + state.opening >= MAX_SESSIONS {
                        return Err(Fault::new(
                            "host_capacity",
                            "close another host before opening this one",
                        ));
                    }
                    state.opening += 1;
                }
                // Reservation drops even when a handshake is cancelled.
                struct Opening<'a>(&'a Gateway);
                impl Drop for Opening<'_> {
                    fn drop(&mut self) {
                        self.0.state.lock().expect("gateway sessions").opening -= 1;
                    }
                }
                let _opening = Opening(self);
                if too_late(deadline) {
                    return Err(not_sent_fault());
                }
                let session = uuid::Uuid::new_v4().to_string();
                let mut dial_route = route.clone();
                dial_route.client_id = client_identity(device, &p.route_id, &route);
                let mut ended = self.ended.subscribe();
                // Dropping the dial ends its process (see `Dialed`).
                let opened = tokio::select! {
                    biased;
                    _ = ended.wait_for(|closed| *closed) => return Err(closed()),
                    _ = until(deadline) => Err(ssh::dial_error(ssh::DialFailure::HandshakeTimeout, String::new())),
                    opened = ssh::GatewayLink::open(&dial_route, &session) => opened,
                };
                let (link, mut features) = opened.map_err(|error| {
                    let fault = open_fault(&error);
                    eprintln!(
                        "{}",
                        crate::log_safe(&format!(
                            "Remote device {device} could not open host route {}: {} ({})",
                            p.route_id,
                            fault.code,
                            ssh::dial_failure(&error).map_or("other", ssh::DialFailure::code),
                        ))
                    );
                    fault
                })?;
                self.check(device)?;
                if self.route(&p.route_id, &p.host_id)? != route {
                    return Err(Fault::new(
                        "host_changed",
                        "the SSH route changed while connecting",
                    ));
                }
                if let Some(features) = features.as_object_mut() {
                    features.remove("pty");
                    features.remove("deflate");
                    features.remove("hosts");
                    features.remove("forwarded_sessions");
                }
                let mut state = self.state.lock().expect("gateway sessions");
                if self.closed.load(Ordering::Acquire) {
                    return Err(closed());
                }
                state.peers.insert(
                    session.clone(),
                    Arc::new(Peer {
                        route_id: p.route_id.clone(),
                        route,
                        link,
                        features: features.clone(),
                    }),
                );
                self.wake.notify_one();
                Ok(
                    json!({"session":session,"route_id":p.route_id,"host_id":p.host_id,"features":features}),
                )
            }
            "hosts.close" => {
                let p: Close = params(&request.params)?;
                uuid(&p.session)?;
                let peer = self
                    .state
                    .lock()
                    .expect("gateway sessions")
                    .peers
                    .remove(&p.session)
                    .ok_or_else(closed)?;
                peer.link.close();
                Ok(json!({}))
            }
            "hosts.call" => {
                let p: Call = params(&request.params)?;
                uuid(&p.request_id)?;
                if !allowed(&p.method) || !p.params.is_object() {
                    return Err(Fault::new(
                        "invalid_request",
                        "this method cannot be forwarded",
                    ));
                }
                let peer = self.peer(&p.session)?;
                self.check(device)?;
                // Nothing has gone out yet. From here on that can no longer be said.
                if too_late(deadline) {
                    return Err(not_sent_fault());
                }
                let read = known_read(&request.params);
                let call = peer.link.call(p.request_id, p.method, p.params);
                let answered = match deadline {
                    Some(at) => tokio::time::timeout_at(at.into(), call).await,
                    None => Ok(call.await),
                };
                // Any of these beats the phone's own timer, which would end every route.
                // `host_timeout` costs it only this request; for the other two it drops
                // this route. A change may have been carried out by now. A read has done
                // no harm, and it says something about the route only if the link has
                // gone quiet as well: pongs show a host that is merely slow to answer.
                let Ok(answered) = answered else {
                    return Err(if !read {
                        Fault::new(
                            "delivery_uncertain",
                            "this host did not answer in time; check the host before repeating a change",
                        )
                    } else if peer.link.heard_lately() {
                        Fault::new("host_timeout", "this host did not answer in time")
                    } else {
                        Fault::new("host_unavailable", "this host has stopped answering")
                    });
                };
                let response = answered
                    .map_err(|error| match error {
                        crate::client::CallError::Invalid(_) => Fault::new("invalid_request", "the forwarded request was not sent"),
                        _ if read => Fault::new("host_unavailable", "the SSH connection ended before the response"),
                        _ => Fault::new("delivery_uncertain", "the SSH connection ended before the response; check the host before repeating a change"),
                    })?;
                // After dispatch, losing trust/session cannot claim the action
                // failed: the host may have completed it before losing the reply.
                if self.check(device).is_err() || self.peer(&p.session).is_err() {
                    return Err(if read {
                        Fault::new(
                            "host_unavailable",
                            "the host session changed after dispatch",
                        )
                    } else {
                        Fault::new(
                            "delivery_uncertain",
                            "the host session changed after dispatch; check the host before repeating a change",
                        )
                    });
                }
                Ok(response)
            }
            _ => Err(Fault::new("invalid_request", "unsupported host method")),
        }
    }
    pub async fn handle(
        &self,
        device: &str,
        request: &Request,
        deadline: Option<Instant>,
    ) -> Value {
        match self.dispatch(device, request, deadline).await {
            Ok(mut response) if request.method == "hosts.call" => {
                // Preserve downstream RPC errors and normal result semantics,
                // while correlating the first-hop envelope to its outer UUID.
                response["id"] = json!(request.id);
                response
            }
            Ok(result) => {
                json!({"v":1,"type":"response","id":request.id,"ok":true,"result":result})
            }
            Err(fault) => crate::rpc::error(&request.id, &fault.code, fault.message),
        }
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        self.close_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::HostRecord;

    fn fixture() -> (tempfile::TempDir, Gateway, HostRecord) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::at(dir.path().into()).unwrap();
        let phone = storage
            .pair(
                "wss://example.com/v1/ws".into(),
                "phone".into(),
                false,
                &dir.path().join("phone.json"),
                None,
            )
            .unwrap();
        let peer = HostRecord {
            id: uuid::Uuid::new_v4().to_string(),
            pairing: None,
            ssh: Some(ssh::Route {
                target: "never-dial-this-fixture".into(),
                remote_bin: "/private/remote".into(),
                somtus: Some("/private/cli".into()),
                home: Some("/private/home".into()),
                host_id: uuid::Uuid::new_v4().to_string(),
                client_id: uuid::Uuid::new_v4().to_string(),
            }),
            label: "My host".into(),
            relay: String::new(),
            route: String::new(),
            added_at_unix: 1,
            allow_insecure_loopback: false,
        };
        HostsFile {
            v: 1,
            hosts: vec![peer.clone()],
        }
        .save(&storage)
        .unwrap();
        (dir, Gateway::new(storage, phone.device_id), peer)
    }
    fn request(method: &str, params: Value) -> Request {
        Request {
            v: 1,
            kind: "request".into(),
            id: uuid::Uuid::new_v4().to_string(),
            method: method.into(),
            params,
        }
    }
    #[test]
    fn a_forwarded_request_is_timed_by_the_phones_timer_for_its_inner_method() {
        let seconds = Duration::from_secs;
        for (method, params, timer) in [
            ("projects.list", json!({}), 15),
            ("dashboard.get", json!({}), 15),
            ("upload.chunk", json!({}), 15),
            ("upload.cancel", json!({}), 15),
            ("shell.keys", json!({}), 15),
            ("shell.input", json!({}), 15),
            ("shell.resize", json!({}), 15),
            ("shell.resize.clear", json!({}), 15),
            (
                "native.invoke",
                json!({"module":"Store","method":"renameProject","args":{}}),
                15,
            ),
            ("shell.close", json!({}), 30),
            ("chat.stop", json!({}), 30),
            ("upload.begin", json!({}), 30),
            ("upload.finish", json!({}), 30),
            ("shell.paste", json!({}), 30),
            ("project.memo.set", json!({}), 30),
            ("chat.command", json!({}), 60),
            ("shell.create", json!({}), 90),
            ("project.create", json!({}), 90),
            ("orchestrator.create", json!({}), 90),
            ("chat.create", json!({}), 90),
            ("chat.asset.read", json!({"offset":0}), 90),
            ("chat.asset.read", json!({"offset":4096}), 15),
            ("chat.asset.read", json!({}), 15),
            ("shell.output", json!({}), 15),
            ("shell.output", json!({"wait_ms":0}), 15),
            ("shell.output", json!({"wait_ms":8000}), 28),
            ("shell.output", json!({"wait_ms":10000}), 30),
            ("shell.output", json!({"wait_ms":60000}), 30),
            ("shell.output", json!({"wait_ms":-1}), 15),
            ("shell.output", json!({"wait_ms":"8000"}), 15),
            ("chat.events", json!({"wait_ms":0}), 15),
            ("chat.events", json!({"wait_ms":20000}), 40),
            ("chat.events", json!({"wait_ms":25000}), 45),
            ("chat.events", json!({"wait_ms":90000}), 45),
            ("state.watch", json!({"wait_ms":0}), 15),
            ("state.watch", json!({"wait_ms":25000}), 45),
        ] {
            assert_eq!(
                phone_timer(method, &params),
                seconds(timer),
                "{method} {params}"
            );
            let received = Instant::now();
            let session = uuid::Uuid::new_v4().to_string();
            let call = json!({"method":"hosts.call","params":{"session":session,"request_id":"r","method":method,"params":params}});
            assert_eq!(
                deadline(&Target::Session(session), &call, received),
                Some(received + seconds(timer) - seconds(2)),
                "{method} {params}"
            );
        }
        let received = Instant::now();
        let open = json!({"method":"hosts.open","params":{"route_id":"r","host_id":"h"}});
        assert_eq!(
            deadline(&Target::Route("r".into()), &open, received),
            Some(received + seconds(28))
        );
        assert_eq!(deadline(&Target::Local, &open, received), None);
    }
    #[test]
    fn a_queued_request_for_this_machine_goes_by_the_phones_timer_and_only_a_phones_does() {
        let seconds = Duration::from_secs;
        let received = Instant::now();
        for (method, params, lane, timer) in [
            ("shell.keys", json!({}), Lane::Ordered, 15),
            ("shell.input", json!({}), Lane::Ordered, 15),
            ("shell.resize", json!({}), Lane::Ordered, 15),
            ("shell.resize.clear", json!({}), Lane::Ordered, 15),
            ("shell.close", json!({}), Lane::Ordered, 30),
            ("chat.command", json!({}), Lane::Ordered, 60),
            ("shell.create", json!({}), Lane::Ordered, 90),
            ("hosts.list", json!({}), Lane::Read, 15),
            ("projects.list", json!({}), Lane::Read, 15),
            ("chat.asset.read", json!({"offset":0}), Lane::Read, 90),
            ("shell.output", json!({"wait_ms":8000}), Lane::LongPoll, 28),
            ("chat.events", json!({"wait_ms":25000}), Lane::LongPoll, 45),
        ] {
            let request = json!({"method":method,"params":params});
            assert_eq!(
                queue_deadline(&Target::Local, lane, false, &request, received),
                Some(received + seconds(timer) - seconds(2)),
                "{method} {params}"
            );
            // What a desktop waits for is not a phone's timer.
            assert_eq!(
                queue_deadline(&Target::Local, lane, true, &request, received),
                None,
                "{method} {params}"
            );
            // Running, it is left to the limits of its CLI.
            assert_eq!(deadline(&Target::Local, &request, received), None);
        }
        // A terminal stream's lanes are never on a phone's clock, whoever asks.
        for lane in [Lane::Attach, Lane::Stream] {
            let request = json!({"method":"pty.read","params":{}});
            assert_eq!(
                queue_deadline(&Target::Local, lane, false, &request, received),
                None
            );
        }
    }
    /// One still queued when it could only be refused once started would free its target's
    /// slot for what was sent after it, and that would reach the host first.
    #[test]
    fn a_request_for_a_host_leaves_the_queue_before_it_could_only_be_refused_at_dispatch() {
        let received = Instant::now();
        let session = uuid::Uuid::new_v4().to_string();
        let call = |method: &str| json!({"method":"hosts.call","params":{"session":session,"request_id":"r","method":method,"params":{}}});
        let open = json!({"method":"hosts.open","params":{"route_id":"r","host_id":"h"}});
        for (target, request) in [
            (Target::Session(session.clone()), call("shell.keys")),
            (Target::Session(session.clone()), call("chat.command")),
            (Target::Route("r".into()), open),
        ] {
            let leaves = queue_deadline(&target, Lane::Ordered, false, &request, received);
            let answered_by = deadline(&target, &request, received);
            assert_eq!(
                leaves.map(|at| at + LEAST_TO_START),
                answered_by,
                "{request}"
            );
            // A desktop asking makes no difference to what goes to a host.
            assert_eq!(
                queue_deadline(&target, Lane::Ordered, true, &request, received),
                leaves,
                "{request}"
            );
        }
    }
    #[test]
    fn only_a_method_that_cannot_change_anything_is_a_known_read() {
        let native = |module: &str, method: &str| json!({"method":"native.invoke","params":{"module":module,"method":method,"args":{}}});
        let plain = |method: &str, params: Value| json!({"method":method,"params":params});
        for read in [
            plain("projects.list", json!({})),
            plain("worktrees.list", json!({})),
            plain("tasks.list", json!({})),
            plain("shells.list", json!({})),
            plain("orchestrators.list", json!({})),
            plain("shell.output", json!({})),
            plain("shell.output", json!({"if_changed":"h","wait_ms":8000})),
            plain("shell.history", json!({})),
            plain("appearance.get", json!({})),
            plain("dashboard.get", json!({})),
            plain("state.watch", json!({"wait_ms":25000})),
            plain("project.memo.get", json!({})),
            plain("tabs.list", json!({})),
            plain("chats.list", json!({})),
            plain("chat.snapshot", json!({})),
            plain("chat.asset.read", json!({"offset":0})),
            plain("chat.events", json!({"wait_ms":25000})),
            plain("chat.models", json!({})),
            plain("chat.suggestions", json!({})),
            native("Store", "snapshot"),
            native("Hosts", "local"),
        ] {
            assert!(known_read(&read), "{read}");
        }
        for change in [
            plain("shell.keys", json!({})),
            plain("shell.input", json!({})),
            plain("shell.resize", json!({})),
            plain("shell.resize.clear", json!({})),
            plain("shell.create", json!({})),
            plain("shell.close", json!({})),
            plain("shell.paste", json!({})),
            plain("project.create", json!({})),
            plain("project.memo.set", json!({})),
            plain("orchestrator.create", json!({})),
            plain("tabs.update", json!({})),
            plain("tabs.open", json!({})),
            plain("chat.create", json!({})),
            plain("chat.command", json!({})),
            plain("chat.stop", json!({})),
            // In the shared lane, and they write to disk all the same.
            plain("upload.begin", json!({})),
            plain("upload.chunk", json!({})),
            plain("upload.finish", json!({})),
            plain("upload.cancel", json!({})),
            native("Store", "renameProject"),
            // A newer phone and host may know this one, and whether it writes.
            native("Store", "aNewerMethod"),
            plain("native.invoke", json!({})),
            plain("native.invoke", Value::Null),
            // What cannot be forwarded is known to be nothing.
            plain("nope", json!({})),
            plain("pty.open", json!({})),
            Value::Null,
        ] {
            assert!(!known_read(&change), "{change}");
        }
    }
    /// A session of `gateway` on a link whose far end is `script`.
    async fn session_on(
        gateway: &Gateway,
        record: &HostRecord,
        script: &str,
        ping: Duration,
    ) -> String {
        let route = record.ssh.clone().unwrap();
        let link = ssh::GatewayLink::to_script(&route, script, ping).await;
        let session = uuid::Uuid::new_v4().to_string();
        gateway.state.lock().unwrap().peers.insert(
            session.clone(),
            Arc::new(Peer {
                route_id: record.id.clone(),
                route,
                link,
                features: json!({}),
            }),
        );
        session
    }
    fn answers_the_dial(record: &HostRecord, then: &str) -> String {
        format!(
            "read hello\necho '{{\"v\":1,\"type\":\"ready\",\"host_id\":\"{}\"}}'\n{then}\n",
            record.ssh.as_ref().unwrap().host_id
        )
    }
    /// Takes every frame and answers none, a ping included.
    fn says_nothing(record: &HostRecord) -> String {
        answers_the_dial(record, "while IFS= read -r frame; do :; done")
    }
    fn forwarded(session: &str, method: &str, params: Value) -> Request {
        request(
            "hosts.call",
            json!({"session":session,"request_id":uuid::Uuid::new_v4().to_string(),"method":method,"params":params}),
        )
    }
    fn says(response: &Value, code: &str, words: &str) {
        assert_eq!(response["error"]["code"], code, "{response}");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains(words),
            "{response}"
        );
    }
    #[tokio::test]
    async fn a_call_with_too_little_time_left_is_not_sent_and_one_with_enough_is() {
        let (dir, gateway, record) = fixture();
        let frames = dir.path().join("frames");
        let far_end = answers_the_dial(
            &record,
            &format!(
                "while IFS= read -r frame; do printf '%s\\n' \"$frame\" >> '{}'; done",
                frames.display()
            ),
        );
        let session = session_on(&gateway, &record, &far_end, Duration::from_secs(5)).await;
        for method in ["projects.list", "shell.keys"] {
            let req = forwarded(&session, method, json!({}));
            let soon = Instant::now() + Duration::from_millis(500);
            let response = gateway.handle(&gateway.device, &req, Some(soon)).await;
            says(&response, "host_timeout", "not sent");
        }
        // Long enough to go out, and it does: the first request the far end has seen.
        let req = forwarded(&session, "projects.list", json!({}));
        let soon = Instant::now() + Duration::from_secs(3);
        let response = gateway.handle(&gateway.device, &req, Some(soon)).await;
        assert_eq!(response["ok"], false, "{response}");
        assert!(!response.to_string().contains("not sent"), "{response}");
        let requests = || {
            std::fs::read_to_string(&frames)
                .unwrap_or_default()
                .lines()
                .filter(|frame| frame.contains("\"type\":\"request\""))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        for _ in 0..500 {
            if !requests().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let seen = requests();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(seen[0].contains("projects.list"), "{seen:?}");
    }
    #[tokio::test]
    async fn a_call_still_out_at_its_deadline_is_answered_by_what_it_can_have_done() {
        let (_dir, gateway, record) = fixture();
        // Its host was heard from just now, so this says nothing against the link.
        let far_end = says_nothing(&record);
        let heard = session_on(&gateway, &record, &far_end, Duration::from_secs(5)).await;
        // This one's pings say its host should have answered long before the deadline.
        let quiet = session_on(&gateway, &record, &far_end, Duration::from_millis(50)).await;
        let newer = json!({"module":"Store","method":"aNewerMethod","args":{}});
        let calls = [
            forwarded(&heard, "projects.list", json!({})),
            forwarded(&heard, "shell.keys", json!({})),
            forwarded(&heard, "upload.chunk", json!({})),
            forwarded(&heard, "native.invoke", newer),
            forwarded(&quiet, "projects.list", json!({})),
            forwarded(&quiet, "shell.keys", json!({})),
        ];
        // Well ahead, so that a slow machine still sends them.
        let soon = Some(Instant::now() + Duration::from_secs(3));
        let answers = futures_util::future::join_all(
            calls
                .iter()
                .map(|call| gateway.handle(&gateway.device, call, soon)),
        )
        .await;
        says(&answers[0], "host_timeout", "did not answer in time");
        for change in &answers[1..4] {
            says(change, "delivery_uncertain", "did not answer in time");
        }
        // A read costs the route only here, and a change is as uncertain as ever.
        says(&answers[4], "host_unavailable", "stopped answering");
        says(&answers[5], "delivery_uncertain", "did not answer in time");
        for answer in &answers {
            assert!(!answer.to_string().contains("not sent"), "{answer}");
        }
    }
    #[tokio::test]
    async fn a_call_lost_with_its_link_is_uncertain_only_if_it_could_have_changed_something() {
        let (_dir, gateway, record) = fixture();
        let far_end = answers_the_dial(
            &record,
            r#"while IFS= read -r frame; do case "$frame" in *'"type":"request"'*) exit 0;; esac; done"#,
        );
        for (method, params, code) in [
            ("projects.list", json!({}), "host_unavailable"),
            ("dashboard.get", json!({}), "host_unavailable"),
            ("shell.keys", json!({}), "delivery_uncertain"),
            ("upload.chunk", json!({}), "delivery_uncertain"),
            (
                "native.invoke",
                json!({"module":"Store","method":"aNewerMethod","args":{}}),
                "delivery_uncertain",
            ),
        ] {
            let session = session_on(&gateway, &record, &far_end, Duration::from_secs(5)).await;
            let call = forwarded(&session, method, params);
            let response = gateway.handle(&gateway.device, &call, None).await;
            assert_eq!(response["error"]["code"], code, "{method}: {response}");
            let peer = gateway.state.lock().unwrap().peers.remove(&session);
            drop(peer);
        }
    }
    #[tokio::test]
    async fn a_call_answered_after_its_route_changed_is_uncertain_only_if_it_was_a_change() {
        let (dir, gateway, record) = fixture();
        let go = dir.path().join("go");
        let far_end = answers_the_dial(
            &record,
            &format!(
                r#"while IFS= read -r frame; do case "$frame" in *'"type":"request"'*)
 while [ ! -e '{go}' ]; do sleep 0.05; done
 id=$(printf '%s' "$frame" | sed 's/.*"id":"\([^"]*\)".*/\1/')
 printf '{{"v":1,"type":"response","id":"%s","ok":true,"result":{{}}}}\n' "$id";; esac; done"#,
                go = go.display()
            ),
        );
        let mut moved = record.clone();
        moved.ssh.as_mut().unwrap().target = "somewhere-else".into();
        for (method, code) in [
            ("projects.list", "host_unavailable"),
            ("shell.keys", "delivery_uncertain"),
            ("upload.chunk", "delivery_uncertain"),
        ] {
            HostsFile {
                v: 1,
                hosts: vec![record.clone()],
            }
            .save(&gateway.storage)
            .unwrap();
            let _ = std::fs::remove_file(&go);
            let session = session_on(&gateway, &record, &far_end, Duration::from_secs(5)).await;
            let call = forwarded(&session, method, json!({}));
            // Joined in this order, the call is out before the route changes.
            let (response, ()) =
                tokio::join!(gateway.handle(&gateway.device, &call, None), async {
                    HostsFile {
                        v: 1,
                        hosts: vec![moved.clone()],
                    }
                    .save(&gateway.storage)
                    .unwrap();
                    std::fs::write(&go, "").unwrap();
                });
            assert_eq!(response["error"]["code"], code, "{method}: {response}");
        }
    }
    #[test]
    fn what_never_started_says_so_under_its_own_id() {
        let id = uuid::Uuid::new_v4().to_string();
        let answer = not_sent(&json!({"id":id,"method":"hosts.call"}));
        assert_eq!(answer["id"], id);
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["error"]["code"], "host_timeout");
        assert!(
            answer["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not sent")
        );
        for request in [json!({"id":7}), json!({"method":"hosts.call"}), Value::Null] {
            assert_eq!(not_sent(&request)["id"], Value::Null, "{request}");
        }
    }
    #[test]
    fn a_failed_dial_reaches_a_phone_as_a_code_and_fixed_words() {
        use ssh::DialFailure::*;
        for (failure, code) in [
            (IdentityChanged, "host_changed"),
            (SessionBusy, "host_busy"),
            (AuthFailed, "host_login_failed"),
            (HostKey, "host_login_failed"),
            (NameResolution, "host_unreachable"),
            (Unreachable, "host_unreachable"),
            (NotInstalled, "host_not_installed"),
            (CliMissing, "host_not_installed"),
            (CliUnsupported, "host_not_installed"),
            (HandshakeTimeout, "host_timeout"),
            (NoForwarding, "host_unsupported"),
            (IdentityUnavailable, "host_unavailable"),
            (HomeUnavailable, "host_unavailable"),
            (LoginOutput, "host_unavailable"),
            (SshNotStarted, "host_unavailable"),
            (Closed, "host_unavailable"),
        ] {
            // However far down the chain it is, and whatever was printed.
            let printed = "secret-token at /private/place";
            let error = ssh::dial_error(failure, printed.into()).context("while opening");
            let fault = open_fault(&error);
            assert_eq!(fault.code, code);
            assert!(!fault.message.is_empty());
            assert!(!fault.message.contains("secret"), "{}", fault.message);
            assert!(!fault.message.contains('/'), "{}", fault.message);
        }
        // Not a dial failure, whatever its words: the fallback, and none of them.
        let other = anyhow::anyhow!("forwarded sessions at /private/place");
        let fault = open_fault(&other);
        assert_eq!(fault.code, "host_unavailable");
        assert!(!fault.message.contains("/private"), "{}", fault.message);
        // What the phone is told to do about a changed identity is what will work.
        let changed = open_fault(&ssh::dial_error(IdentityChanged, String::new()));
        assert!(
            changed.message.contains("remove the route"),
            "{}",
            changed.message
        );
        assert!(changed.message.contains("gateway"), "{}", changed.message);
    }
    #[tokio::test]
    async fn an_open_past_its_deadline_starts_no_dial() {
        let (_dir, gateway, record) = fixture();
        let req = request(
            "hosts.open",
            json!({"route_id":record.id,"host_id":record.ssh.as_ref().unwrap().host_id}),
        );
        let late = Instant::now() - Duration::from_millis(1);
        let response = gateway.handle(&gateway.device, &req, Some(late)).await;
        assert_eq!(response["error"]["code"], "host_timeout", "{response}");
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not sent"),
            "{response}"
        );
        assert_eq!(gateway.state.lock().unwrap().opening, 0);
        // Nor with too little left for one to finish.
        let soon = Instant::now() + Duration::from_millis(500);
        let response = gateway.handle(&gateway.device, &req, Some(soon)).await;
        says(&response, "host_timeout", "not sent");
        assert_eq!(gateway.state.lock().unwrap().opening, 0);
    }
    #[test]
    fn a_request_is_started_only_with_about_a_second_left() {
        let now = Instant::now();
        assert!(!too_late(None));
        assert!(!too_late(Some(now + Duration::from_secs(5))));
        for gone in [
            now - Duration::from_secs(1),
            now,
            now + Duration::from_millis(500),
        ] {
            assert!(too_late(Some(gone)), "{:?}", gone.duration_since(now));
        }
    }
    #[test]
    fn downstream_principal_is_stable_and_separates_phone_gateway_route_and_host() {
        let (_dir, gateway, record) = fixture();
        let mut route = record.ssh.unwrap();
        let original = client_identity(&gateway.device, &record.id, &route);
        crate::crypto::uuid(&original).unwrap();
        assert_eq!(
            original,
            client_identity(&gateway.device, &record.id, &route)
        );
        assert_ne!(
            original,
            client_identity(&uuid::Uuid::new_v4().to_string(), &record.id, &route)
        );
        assert_ne!(
            original,
            client_identity(&gateway.device, &uuid::Uuid::new_v4().to_string(), &route)
        );
        route.host_id = uuid::Uuid::new_v4().to_string();
        assert_ne!(
            original,
            client_identity(&gateway.device, &record.id, &route)
        );
        route.client_id = uuid::Uuid::new_v4().to_string();
        assert_ne!(
            original,
            client_identity(&gateway.device, &record.id, &route)
        );
    }
    #[tokio::test]
    async fn listing_is_sanitized_and_authority_and_request_shapes_are_strict() {
        let (_dir, gateway, record) = fixture();
        let response = gateway
            .handle(&gateway.device, &request("hosts.list", json!({})), None)
            .await;
        assert_eq!(
            response["result"],
            json!({"v":1,"hosts":[{"route_id":record.id,"host_id":record.ssh.as_ref().unwrap().host_id,"label":"My host","transport":"ssh"}]})
        );
        assert!(!response.to_string().contains("private"));
        for (method, params) in [
            ("hosts.list", json!({"credentials":true})),
            (
                "hosts.open",
                json!({"route_id":record.id,"host_id":record.ssh.as_ref().unwrap().host_id,"target":"untrusted-host"}),
            ),
            (
                "hosts.call",
                json!({"session":uuid::Uuid::new_v4().to_string(),"request_id":uuid::Uuid::new_v4().to_string(),"method":"hosts.call","params":{}}),
            ),
            (
                "hosts.call",
                json!({"session":uuid::Uuid::new_v4().to_string(),"request_id":uuid::Uuid::new_v4().to_string(),"method":"pty.open","params":{}}),
            ),
            (
                "hosts.call",
                json!({"session":uuid::Uuid::new_v4().to_string(),"request_id":uuid::Uuid::new_v4().to_string(),"method":"link.configure","params":{}}),
            ),
            (
                "hosts.call",
                json!({"session":uuid::Uuid::new_v4().to_string(),"request_id":"not-a-uuid","method":"projects.list","params":{}}),
            ),
        ] {
            let response = gateway
                .handle(&gateway.device, &request(method, params), None)
                .await;
            assert_eq!(response["error"]["code"], "invalid_request", "{response}");
        }
        let req = request("hosts.list", json!({}));
        assert_eq!(
            gateway
                .handle(&uuid::Uuid::new_v4().to_string(), &req, None)
                .await["error"]["code"],
            "host_session_closed"
        );
        gateway.close_all();
        assert_eq!(
            gateway.handle(&gateway.device, &req, None).await["error"]["code"],
            "host_session_closed"
        );
    }
    #[tokio::test]
    async fn removed_and_repinned_routes_fail_before_ssh_is_started() {
        let (_dir, gateway, record) = fixture();
        let req = request(
            "hosts.open",
            json!({"route_id":record.id,"host_id":record.ssh.as_ref().unwrap().host_id}),
        );
        let mut changed = record.clone();
        changed.ssh.as_mut().unwrap().host_id = uuid::Uuid::new_v4().to_string();
        HostsFile {
            v: 1,
            hosts: vec![changed],
        }
        .save(&gateway.storage)
        .unwrap();
        assert_eq!(
            gateway.handle(&gateway.device, &req, None).await["error"]["code"],
            "host_changed"
        );
        HostsFile {
            v: 1,
            hosts: vec![],
        }
        .save(&gateway.storage)
        .unwrap();
        assert_eq!(
            gateway.handle(&gateway.device, &req, None).await["error"]["code"],
            "host_not_found"
        );
    }
}
