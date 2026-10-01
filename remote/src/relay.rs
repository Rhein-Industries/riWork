//! Blind, bounded router. Never parses/logs endpoint plaintext or holds pairing PSKs.
use crate::{
    MAX_FRAME,
    crypto::{decode, uuid},
    log_safe,
};
use anyhow::{Result, ensure};
use axum::{
    Router,
    body::Bytes,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use subtle::ConstantTimeEq;
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::{Duration, Instant, interval_at, sleep_until, timeout},
};

/// Budget and liveness values. The defaults are what docs/remote-deployment.md
/// documents; tests shorten them.
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// Sockets allowed before they authenticate. A newcomer beyond this budget
    /// drops the oldest, so silent sockets can never lock out a registration.
    pub preauth_sockets: usize,
    /// Time from upgrade to a valid registration message.
    pub register_timeout: Duration,
    pub ping_interval: Duration,
    /// A socket that sent nothing at all (pongs count) for this long is closed.
    pub idle_timeout: Duration,
    /// A registration silent this long is replaced by a new one that authenticates
    /// with the same route token; a live one is still rejected as a duplicate.
    pub replace_after: Duration,
}
impl Default for Tuning {
    fn default() -> Self {
        Self {
            preauth_sockets: 16,
            register_timeout: Duration::from_secs(3),
            ping_interval: Duration::from_secs(20),
            idle_timeout: Duration::from_secs(60),
            replace_after: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub route_id: String,
    pub desktop_token_sha256: String,
    pub mobile_token_sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Routes {
    pub v: u8,
    pub routes: Vec<Route>,
}
impl Routes {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.v == 1 && self.routes.len() <= 128,
            "invalid routes version/limit"
        );
        let mut ids = std::collections::HashSet::new();
        for r in &self.routes {
            uuid(&r.route_id)?;
            ensure!(ids.insert(&r.route_id), "duplicate route");
            for h in [&r.desktop_token_sha256, &r.mobile_token_sha256] {
                ensure!(
                    h.len() == 64 && hex::encode(hex::decode(h)?) == *h,
                    "token hash must be lowercase SHA256 hex"
                );
            }
            ensure!(
                r.desktop_token_sha256 != r.mobile_token_sha256,
                "role tokens must differ"
            );
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Register {
    v: u8,
    #[serde(rename = "type")]
    kind: String,
    route_id: String,
    role: String,
    token: String,
}
struct Peer {
    id: uuid::Uuid,
    tx: mpsc::Sender<Message>,
    /// Setting a reason ends the socket's task with it.
    close: watch::Sender<Option<&'static str>>,
    /// When anything last arrived on the socket; pongs count.
    seen: Arc<Mutex<Instant>>,
}
impl Peer {
    fn silent_for(&self) -> Duration {
        self.seen.lock().expect("liveness mutex").elapsed()
    }
}
#[derive(Default)]
struct PreAuth {
    next: u64,
    /// Oldest first; the sender tells that socket it was dropped for a newer one.
    waiting: BTreeMap<u64, oneshot::Sender<()>>,
}
/// Holds a pre-authentication place until the socket registers or ends.
struct Admission {
    shared: Arc<Shared>,
    id: u64,
}
impl Drop for Admission {
    fn drop(&mut self) {
        let mut pre = self.shared.preauth.lock().expect("preauth mutex");
        pre.waiting.remove(&self.id);
    }
}
struct RejectLog {
    since: Instant,
    logged: u32,
    suppressed: u32,
}
struct Shared {
    routes: HashMap<String, Route>,
    /// Authenticated sockets only; its length is what `max_connections` limits.
    peers: Mutex<HashMap<(String, String), Peer>>,
    max_connections: usize,
    tuning: Tuning,
    preauth: Mutex<PreAuth>,
    rejects: Mutex<RejectLog>,
}
enum Admit {
    Registered { peer_online: bool },
    Duplicate,
    Full,
}
/// Accepts connections with Nagle's algorithm off. Every frame is one small
/// write that someone is waiting for; left to Nagle, a frame that follows
/// another before the peer has acknowledged it is held back for the peer's
/// delayed ACK, up to tens of milliseconds on a local hop.
fn nodelay(
    listener: tokio::net::TcpListener,
) -> impl axum::serve::Listener<Io = tokio::net::TcpStream, Addr = SocketAddr> {
    axum::serve::ListenerExt::tap_io(listener, |tcp| {
        let _ = tcp.set_nodelay(true);
    })
}
#[derive(Clone)]
pub struct Relay {
    state: Arc<Shared>,
}
impl Relay {
    pub fn new(routes: Routes, max_connections: usize) -> Result<Self> {
        Self::with_tuning(routes, max_connections, Tuning::default())
    }
    pub fn with_tuning(routes: Routes, max_connections: usize, tuning: Tuning) -> Result<Self> {
        routes.validate()?;
        ensure!(
            (1..=256).contains(&max_connections),
            "connection limit 1..256"
        );
        ensure!(
            tuning.preauth_sockets >= 1,
            "pre-authentication budget must allow at least one socket"
        );
        Ok(Self {
            state: Arc::new(Shared {
                routes: routes
                    .routes
                    .into_iter()
                    .map(|r| (r.route_id.clone(), r))
                    .collect(),
                peers: Mutex::new(HashMap::new()),
                max_connections,
                tuning,
                preauth: Mutex::new(PreAuth::default()),
                rejects: Mutex::new(RejectLog {
                    since: Instant::now(),
                    logged: 0,
                    suppressed: 0,
                }),
            }),
        })
    }
    pub fn router(&self) -> Router {
        Router::new()
            .route("/healthz", get(|| async { (StatusCode::OK, "ok\n") }))
            .route("/v1/ws", get(upgrade))
            .with_state(self.clone())
    }
    pub async fn serve(self, bind: SocketAddr) -> Result<()> {
        ensure!(
            bind.ip().is_loopback(),
            "plaintext relay must bind loopback; use a TLS reverse proxy for deployment"
        );
        let listener = tokio::net::TcpListener::bind(bind).await?;
        axum::serve(nodelay(listener), self.router()).await?;
        Ok(())
    }
    /// Why a registration is not authorized. Never includes anything the sender chose.
    fn check(&self, r: &Register) -> std::result::Result<(), &'static str> {
        if r.v != 1 || r.kind != "register" {
            return Err("unsupported version or message type");
        }
        let Some(route) = self.state.routes.get(&r.route_id) else {
            return Err("unknown route");
        };
        let hash = match r.role.as_str() {
            "desktop" => &route.desktop_token_sha256,
            "mobile" => &route.mobile_token_sha256,
            _ => return Err("unknown role"),
        };
        let Ok(token) = decode::<32>(&r.token) else {
            return Err("malformed token");
        };
        let Ok(expected) = hex::decode(hash) else {
            return Err("unusable route hash");
        };
        if bool::from(Sha256::digest(token).as_slice().ct_eq(&expected)) {
            Ok(())
        } else {
            Err("token mismatch")
        }
    }
    /// Registration failures are attacker-driven, so their log lines are bounded.
    fn reject(&self, reason: &str) {
        let mut log = self.state.rejects.lock().expect("reject log mutex");
        if log.since.elapsed() >= Duration::from_secs(10) {
            if log.suppressed > 0 {
                eprintln!(
                    "relay: {} more rejected registrations not logged",
                    log.suppressed
                );
            }
            *log = RejectLog {
                since: Instant::now(),
                logged: 0,
                suppressed: 0,
            };
        }
        if log.logged < 20 {
            log.logged += 1;
            eprintln!("relay: registration rejected: {reason}");
        } else {
            log.suppressed += 1;
        }
    }
    fn enter_preauth(&self) -> (Admission, oneshot::Receiver<()>) {
        let (tx, rx) = oneshot::channel();
        let mut pre = self.state.preauth.lock().expect("preauth mutex");
        while pre.waiting.len() >= self.state.tuning.preauth_sockets {
            if let Some((_, oldest)) = pre.waiting.pop_first() {
                let _ = oldest.send(());
            }
        }
        let id = pre.next;
        pre.next += 1;
        pre.waiting.insert(id, tx);
        drop(pre);
        (
            Admission {
                shared: self.state.clone(),
                id,
            },
            rx,
        )
    }
    /// Registers an already authenticated socket, replacing a silent one.
    fn admit(&self, key: &(String, String), other: &(String, String), peer: Peer) -> Result<Admit> {
        let mut peers = self.state.peers.lock().expect("peer mutex");
        if let Some(old) = peers.get(key) {
            if old.silent_for() < self.state.tuning.replace_after {
                return Ok(Admit::Duplicate);
            }
            // The newcomer just proved it holds this role's token. A registration
            // that stopped answering (network switch, sleeping Mac) must not lock
            // the real endpoint out until the idle timeout notices.
            if let Some(old) = peers.remove(key) {
                let _ = old.close.send(Some("replaced by a newer registration"));
                eprintln!(
                    "relay: replacing silent {} socket on route {}",
                    key.1, key.0
                );
                notify_gone(&peers, other);
            }
        }
        if peers.len() >= self.state.max_connections {
            return Ok(Admit::Full);
        }
        let peer_online = peers.contains_key(other);
        if let Some(peer) = peers.get(other) {
            // Same FIFO as endpoint frames: peer notification must precede
            // any new peer handshake, even under concurrent connections.
            peer.tx
                .try_send(control("peer", "online", true))
                .map_err(|_| anyhow::anyhow!("peer queue full"))?;
        }
        peers.insert(key.clone(), peer);
        Ok(Admit::Registered { peer_online })
    }
}
/// Tells `other` that its peer is gone, or closes it if that cannot be queued.
fn notify_gone(peers: &HashMap<(String, String), Peer>, other: &(String, String)) {
    if let Some(peer) = peers.get(other)
        && peer.tx.try_send(control("peer", "online", false)).is_err()
    {
        // Do not leave stale authenticated state at a slow endpoint if
        // the ordered peer-loss control itself cannot fit in its queue.
        let _ = peer
            .close
            .send(Some("peer-loss notification did not fit its queue"));
    }
}
async fn upgrade(State(relay): State<Relay>, ws: WebSocketUpgrade) -> axum::response::Response {
    // Taken before the upgrade completes: no socket exists outside the budget.
    let (admission, evicted) = relay.enter_preauth();
    ws.read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_FRAME * 2)
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| route_socket(relay, socket, admission, evicted))
        .into_response()
}
fn control(kind: &str, field: &str, value: bool) -> Message {
    Message::Text(
        serde_json::json!({"v":1,"type":kind,field:value})
            .to_string()
            .into(),
    )
}
async fn route_socket(
    relay: Relay,
    mut ws: WebSocket,
    admission: Admission,
    mut evicted: oneshot::Receiver<()>,
) {
    let t = relay.state.tuning;
    let first = tokio::select! {
        _ = &mut evicted => {
            relay.reject("unauthenticated socket dropped for a newer one");
            return;
        }
        first = timeout(t.register_timeout, ws.recv()) => first,
    };
    let text = match first {
        Ok(Some(Ok(Message::Text(text)))) => text,
        Err(_) => {
            relay.reject("no registration before the timeout");
            return;
        }
        Ok(_) => {
            relay.reject("first message was not text");
            return;
        }
    };
    // Registration is much smaller than content frames and never echoed/logged.
    if text.len() > 2048 {
        relay.reject("registration too large");
        return;
    }
    let Ok(r) = serde_json::from_str::<Register>(&text) else {
        relay.reject("malformed registration");
        return;
    };
    if let Err(why) = relay.check(&r) {
        relay.reject(why);
        let _ = ws.close().await;
        return;
    }
    drop(admission);
    // Authenticated from here on, so route and role are safe to log.
    let (route, role) = (r.route_id.as_str(), r.role.as_str());
    let key = (r.route_id.clone(), r.role.clone());
    let other = (
        r.route_id.clone(),
        if r.role == "desktop" {
            "mobile"
        } else {
            "desktop"
        }
        .to_owned(),
    );
    let id = uuid::Uuid::new_v4();
    let (tx, mut rx) = mpsc::channel::<Message>(16);
    let (close, mut cancel) = watch::channel::<Option<&'static str>>(None);
    let seen = Arc::new(Mutex::new(Instant::now()));
    let peer = Peer {
        id,
        tx,
        close,
        seen: seen.clone(),
    };
    let peer_online = match relay.admit(&key, &other, peer) {
        Ok(Admit::Registered { peer_online }) => peer_online,
        Ok(Admit::Duplicate) => {
            relay.reject(&format!("duplicate {role} registration on route {route}"));
            let _ = ws.close().await;
            return;
        }
        Ok(Admit::Full) => {
            let limit = relay.state.max_connections;
            relay.reject(&format!("relay full ({limit} authenticated sockets)"));
            let _ = ws.close().await;
            return;
        }
        Err(e) => {
            relay.reject(&format!("{role} registration on route {route}: {e}"));
            return;
        }
    };
    // Always remove slot on send/recv error, including failed registration ACK.
    let result=async {
        ws.send(control("registered","peer_online",peer_online)).await?;
        let (mut sink,mut source)=ws.split();
        let mut ping=interval_at(Instant::now()+t.ping_interval,t.ping_interval);
        loop {
            let idle_at=*seen.lock().expect("liveness mutex")+t.idle_timeout;
            tokio::select! {
                _=cancel.changed()=>{
                    let why=(*cancel.borrow()).unwrap_or("registration removed");
                    anyhow::bail!(why)
                },
                _=sleep_until(idle_at)=>anyhow::bail!("idle timeout: nothing received"),
                _=ping.tick()=>{timeout(Duration::from_secs(10),sink.send(Message::Ping(Bytes::new()))).await??;},
                outgoing=rx.recv()=>match outgoing {
                    Some(msg)=>{timeout(Duration::from_secs(10),sink.send(msg)).await??;},None=>break,
                },
                incoming=source.next()=>match incoming {
                    Some(Ok(frame))=>{
                        *seen.lock().expect("liveness mutex")=Instant::now();
                        match frame {
                            Message::Text(text)=>{
                                ensure!(text.len()<=MAX_FRAME,"frame limit");
                                // The router does not deserialize or inspect endpoint messages.
                                let peers=relay.state.peers.lock().expect("peer mutex");
                                let target=peers.get(&other).ok_or_else(||anyhow::anyhow!("peer unavailable"))?;
                                target.tx.try_send(Message::Text(text)).map_err(|_|anyhow::anyhow!("peer queue full"))?;
                            },
                            Message::Ping(p)=>{timeout(Duration::from_secs(10),sink.send(Message::Pong(p))).await??;},
                            Message::Pong(_)=>{},
                            Message::Close(_)=>break,
                            Message::Binary(_)=>anyhow::bail!("unsupported binary frame"),
                        }
                    },
                    Some(Err(e))=>anyhow::bail!("socket error: {e}"),
                    None=>break,
                }
            }
        } Ok::<_,anyhow::Error>(())
    }.await;
    if let Err(e) = &result {
        eprintln!(
            "relay: {role} socket on route {route} closed: {}",
            log_safe(&format!("{e:#}"))
        );
    }
    let mut peers = relay.state.peers.lock().expect("peer mutex");
    if peers.get(&key).is_some_and(|p| p.id == id) {
        peers.remove(&key);
        notify_gone(&peers, &other);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        connector::{connect_registered, receive_json},
        crypto::{b64, random32},
    };
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    #[tokio::test]
    async fn sockets_the_relay_accepts_and_the_connector_opens_have_nagle_off() {
        use axum::serve::Listener;
        // The relay's side: what `serve` accepts.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut listener = nodelay(listener);
        let client = tokio::spawn(async move { tokio::net::TcpStream::connect(addr).await });
        let (accepted, _) = listener.accept().await;
        assert!(accepted.nodelay().unwrap());
        client.await.unwrap().unwrap();
        // The endpoint's side: what `connect_registered` opens.
        let m = random32();
        let route = uuid::Uuid::new_v4().to_string();
        let relay = Relay::new(
            Routes {
                v: 1,
                routes: vec![Route {
                    route_id: route.clone(),
                    desktop_token_sha256: hex::encode(Sha256::digest(random32())),
                    mobile_token_sha256: hex::encode(Sha256::digest(m)),
                }],
            },
            8,
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, relay.router()).await.unwrap();
        });
        let (socket, _) = connect_registered(&url, &route, "mobile", &b64(&m))
            .await
            .unwrap();
        assert!(socket.get_ref().get_ref().nodelay().unwrap());
        task.abort();
    }
    #[tokio::test]
    async fn full_peer_queue_closes_sender_without_unbounded_buffering() {
        let d = random32();
        let m = random32();
        let route = uuid::Uuid::new_v4().to_string();
        let relay = Relay::new(
            Routes {
                v: 1,
                routes: vec![Route {
                    route_id: route.clone(),
                    desktop_token_sha256: hex::encode(Sha256::digest(d)),
                    mobile_token_sha256: hex::encode(Sha256::digest(m)),
                }],
            },
            8,
        )
        .unwrap();
        // A deliberately stalled destination exercises the actual networking
        // forwarding path deterministically, without OS socket-buffer timing.
        let (tx, _stalled_rx) = mpsc::channel(16);
        let (close, cancel) = watch::channel(None);
        relay.state.peers.lock().unwrap().insert(
            (route.clone(), "desktop".into()),
            Peer {
                id: uuid::Uuid::new_v4(),
                tx,
                close,
                seen: Arc::new(Mutex::new(Instant::now())),
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, relay.router()).await.unwrap();
        });
        let (mut mobile, _) =
            connect_registered(&format!("ws://{addr}/v1/ws"), &route, "mobile", &b64(&m))
                .await
                .unwrap();
        for _ in 0..16 {
            let _ = mobile.send(ClientMessage::Text("opaque".into())).await;
        }
        assert!(
            timeout(Duration::from_secs(2), receive_json(&mut mobile))
                .await
                .unwrap()
                .is_err()
        );
        assert!(
            cancel.borrow().is_some(),
            "a full peer-loss queue must force the destination closed"
        );
        task.abort();
    }
}
