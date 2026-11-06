//! Phone access to explicitly registered SSH hosts through its paired desktop.
//! The first-hop session owns every downstream link. Nothing supplied by a
//! phone becomes an SSH address, command, credential or mutable route target.
use crate::{client::HostsFile, config::Storage, rpc::Request, ssh};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_SESSIONS: usize = 4;

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
    // The complete immutable registry route, including paths and account.
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
    async fn dispatch(&self, device: &str, request: &Request) -> Result<Value, Fault> {
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
                let session = uuid::Uuid::new_v4().to_string();
                let mut dial_route = route.clone();
                dial_route.client_id = client_identity(device, &p.route_id, &route);
                let mut ended = self.ended.subscribe();
                let opened = tokio::select! {
                    biased;
                    _ = ended.wait_for(|closed| *closed) => return Err(closed()),
                    opened = ssh::GatewayLink::open(&dial_route, &session) => opened,
                };
                let (link, mut features) = opened.map_err(|error| {
                    let code = if error.to_string().contains("forwarded sessions") {
                        "host_unsupported"
                    } else { "host_unavailable" };
                    Fault::new(code, "cannot open this SSH host; check its connection and update Somtus on the host")
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
                let response = peer.link.call(p.request_id, p.method, p.params).await
                    .map_err(|error| match error {
                        crate::client::CallError::Invalid(_) => Fault::new("invalid_request", "the forwarded request was not sent"),
                        _ => Fault::new("delivery_uncertain", "the SSH connection ended before the response; check the host before repeating a change"),
                    })?;
                // After dispatch, losing trust/session cannot claim the action
                // failed: the host may have completed it before losing the reply.
                if self.check(device).is_err() || self.peer(&p.session).is_err() {
                    return Err(Fault::new(
                        "delivery_uncertain",
                        "the host session changed after dispatch; check the host before repeating a change",
                    ));
                }
                Ok(response)
            }
            _ => Err(Fault::new("invalid_request", "unsupported host method")),
        }
    }
    pub async fn handle(&self, device: &str, request: &Request) -> Value {
        match self.dispatch(device, request).await {
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
            .handle(&gateway.device, &request("hosts.list", json!({})))
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
                .handle(&gateway.device, &request(method, params))
                .await;
            assert_eq!(response["error"]["code"], "invalid_request", "{response}");
        }
        let req = request("hosts.list", json!({}));
        assert_eq!(
            gateway
                .handle(&uuid::Uuid::new_v4().to_string(), &req)
                .await["error"]["code"],
            "host_session_closed"
        );
        gateway.close_all();
        assert_eq!(
            gateway.handle(&gateway.device, &req).await["error"]["code"],
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
            gateway.handle(&gateway.device, &req).await["error"]["code"],
            "host_changed"
        );
        HostsFile {
            v: 1,
            hosts: vec![],
        }
        .save(&gateway.storage)
        .unwrap();
        assert_eq!(
            gateway.handle(&gateway.device, &req).await["error"]["code"],
            "host_not_found"
        );
    }
}
