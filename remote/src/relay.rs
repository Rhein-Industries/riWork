//! Blind, bounded router. Never parses/logs endpoint plaintext or holds pairing PSKs.
use crate::{
    HANDSHAKE_SECONDS, MAX_FRAME,
    crypto::{decode, uuid},
};
use anyhow::{Result, ensure};
use axum::{
    Router,
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
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use subtle::ConstantTimeEq;
use tokio::{
    sync::{Semaphore, mpsc, watch},
    time::{Duration, timeout},
};

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
    close: watch::Sender<bool>,
}
struct Shared {
    routes: HashMap<String, Route>,
    peers: Mutex<HashMap<(String, String), Peer>>,
    slots: Arc<Semaphore>,
}
#[derive(Clone)]
pub struct Relay {
    state: Arc<Shared>,
}
impl Relay {
    pub fn new(routes: Routes, max_connections: usize) -> Result<Self> {
        routes.validate()?;
        ensure!(
            (1..=256).contains(&max_connections),
            "connection limit 1..256"
        );
        Ok(Self {
            state: Arc::new(Shared {
                routes: routes
                    .routes
                    .into_iter()
                    .map(|r| (r.route_id.clone(), r))
                    .collect(),
                peers: Mutex::new(HashMap::new()),
                slots: Arc::new(Semaphore::new(max_connections)),
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
        axum::serve(listener, self.router()).await?;
        Ok(())
    }
    fn authorized(&self, r: &Register) -> bool {
        if r.v != 1 || r.kind != "register" {
            return false;
        }
        let Some(route) = self.state.routes.get(&r.route_id) else {
            return false;
        };
        let hash = match r.role.as_str() {
            "desktop" => &route.desktop_token_sha256,
            "mobile" => &route.mobile_token_sha256,
            _ => return false,
        };
        let Ok(token) = decode::<32>(&r.token) else {
            return false;
        };
        let Ok(expected) = hex::decode(hash) else {
            return false;
        };
        bool::from(Sha256::digest(token).as_slice().ct_eq(&expected))
    }
}
async fn upgrade(State(relay): State<Relay>, ws: WebSocketUpgrade) -> axum::response::Response {
    let Ok(permit) = relay.state.slots.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    ws.read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_FRAME * 2)
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            let _ = route_socket(relay, socket).await;
        })
        .into_response()
}
fn control(kind: &str, field: &str, value: bool) -> Message {
    Message::Text(
        serde_json::json!({"v":1,"type":kind,field:value})
            .to_string()
            .into(),
    )
}
async fn route_socket(relay: Relay, mut ws: WebSocket) -> Result<()> {
    let first = timeout(Duration::from_secs(HANDSHAKE_SECONDS), ws.recv()).await?;
    let Some(Ok(Message::Text(text))) = first else {
        let _ = ws.close().await;
        return Ok(());
    };
    // Registration is much smaller than content frames and never echoed/logged.
    ensure!(text.len() <= 2048, "registration limit");
    let r: Register = serde_json::from_str(&text)?;
    if !relay.authorized(&r) {
        let _ = ws.close().await;
        return Ok(());
    }
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
    let (close, mut cancel) = watch::channel(false);
    let (duplicate, peer_online) = {
        let mut peers = relay.state.peers.lock().expect("peer mutex");
        if peers.contains_key(&key) {
            (true, false)
        } else {
            let peer_online = peers.contains_key(&other);
            if let Some(peer) = peers.get(&other) {
                // Same FIFO as endpoint frames: peer notification must precede
                // any new peer handshake, even under concurrent connections.
                peer.tx
                    .try_send(control("peer", "online", true))
                    .map_err(|_| anyhow::anyhow!("peer queue full"))?;
            }
            peers.insert(key.clone(), Peer { id, tx, close });
            (false, peer_online)
        }
    };
    if duplicate {
        let _ = ws.close().await;
        return Ok(());
    }
    // Always remove slot on send/recv error, including failed registration ACK.
    let result=async {
        ws.send(control("registered","peer_online",peer_online)).await?;
        let (mut sink,mut source)=ws.split();
        loop {
            tokio::select! {
                _=cancel.changed()=>anyhow::bail!("peer notification queue unavailable"),
                outgoing=rx.recv()=>match outgoing {
                    Some(msg)=>{timeout(Duration::from_secs(10),sink.send(msg)).await??;},None=>break,
                },
                incoming=source.next()=>match incoming {
                    Some(Ok(Message::Text(text)))=>{
                        ensure!(text.len()<=MAX_FRAME,"frame limit");
                        // The router does not deserialize or inspect endpoint messages.
                        let peers=relay.state.peers.lock().expect("peer mutex");
                        let target=peers.get(&other).ok_or_else(||anyhow::anyhow!("peer unavailable"))?;
                        target.tx.try_send(Message::Text(text)).map_err(|_|anyhow::anyhow!("peer queue unavailable"))?;
                    },
                    Some(Ok(Message::Ping(p)))=>{timeout(Duration::from_secs(10),sink.send(Message::Pong(p))).await??;},
                    Some(Ok(Message::Pong(_)))=>{},
                    Some(Ok(Message::Close(_)))|None=>break,
                    _=>anyhow::bail!("unsupported frame"),
                }
            }
        } Ok::<_,anyhow::Error>(())
    }.await;
    let mut peers = relay.state.peers.lock().expect("peer mutex");
    if peers.get(&key).is_some_and(|p| p.id == id) {
        peers.remove(&key);
        if let Some(peer) = peers.get(&other)
            && peer.tx.try_send(control("peer", "online", false)).is_err()
        {
            // Do not leave stale authenticated state at a slow endpoint if
            // the ordered peer-loss control itself cannot fit in its queue.
            let _ = peer.close.send(true);
        }
    }
    result
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
        let (close, cancel) = watch::channel(false);
        relay.state.peers.lock().unwrap().insert(
            (route.clone(), "desktop".into()),
            Peer {
                id: uuid::Uuid::new_v4(),
                tx,
                close,
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
            *cancel.borrow(),
            "a full peer-loss queue must force the destination closed"
        );
        task.abort();
    }
}
