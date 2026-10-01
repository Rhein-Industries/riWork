use crate::{
    HANDSHAKE_SECONDS, MAX_FRAME, MAX_PLAINTEXT,
    config::{Device, Storage},
    crypto::{
        ClientFinish, ClientHello, ClientHelloV2, Envelope, PairFinish, PairHello, Pending,
        Session, accept_client_hello_v2, accept_hello, decode, random32,
    },
    lanes::{Lane, Lanes, MAX_QUEUED, classify},
    link::{self, EncodeError},
    log_safe,
    rpc::Rpc,
    viewport::Viewport,
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::{
    sync::watch,
    task::JoinSet,
    time::{Duration, Instant, interval, sleep, timeout},
};
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Selects the process rustls provider before any `ClientConfig` is built.
/// ring is the only provider this crate enables. A second call loses the install
/// race and rustls keeps the first provider.
fn install_ring_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Shipping builds pass `None`, so tungstenite loads the OS root store.
/// `cargo test` on Unix can install one extra root for a local certificate.
fn wss_connector(relay: &str) -> Option<tokio_tungstenite::Connector> {
    #[cfg(all(test, unix))]
    if relay.starts_with("wss://")
        && let Some(config) = TEST_WSS_TRUST.lock().expect("wss trust lock").clone()
    {
        return Some(tokio_tungstenite::Connector::Rustls(config));
    }
    #[cfg(not(all(test, unix)))]
    let _ = relay;
    None
}

#[cfg(all(test, unix))]
static TEST_WSS_TRUST: std::sync::Mutex<Option<Arc<rustls::ClientConfig>>> =
    std::sync::Mutex::new(None);

#[cfg(all(test, unix))]
struct TestWssTrust;

#[cfg(all(test, unix))]
impl TestWssTrust {
    fn install(config: Arc<rustls::ClientConfig>) -> Self {
        let mut slot = TEST_WSS_TRUST.lock().expect("wss trust lock");
        assert!(slot.is_none(), "wss trust override already installed");
        *slot = Some(config);
        Self
    }
}

#[cfg(all(test, unix))]
impl Drop for TestWssTrust {
    fn drop(&mut self) {
        if let Ok(mut slot) = TEST_WSS_TRUST.lock() {
            *slot = None;
        }
    }
}

pub async fn connect_registered(
    relay: &str,
    route: &str,
    role: &str,
    token: &str,
) -> Result<(Socket, bool)> {
    install_ring_provider();
    let config = WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_FRAME * 2)
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    let (mut ws, _) = timeout(
        Duration::from_secs(10),
        // `true` turns Nagle's algorithm off: each frame is a small write that a
        // phone is waiting for, and it must not wait for an ACK of the last one.
        connect_async_tls_with_config(relay, Some(config), true, wss_connector(relay)),
    )
    .await??;
    if relay.starts_with("ws://") {
        // Check the actual TCP destination before transmitting any role token,
        // even if a local hostname resolves unexpectedly.
        ensure!(
            ws.get_ref().get_ref().peer_addr()?.ip().is_loopback(),
            "plaintext connection must terminate on loopback"
        );
    }
    send_json(
        &mut ws,
        &json!({"v":1,"type":"register","route_id":route,"role":role,"token":token}),
    )
    .await?;
    let value = timeout(
        Duration::from_secs(HANDSHAKE_SECONDS),
        receive_json(&mut ws),
    )
    .await
    .context("relay did not acknowledge the registration in time")?
    .context(
        "relay closed the connection during registration (credentials rejected, duplicate registration, or relay full)",
    )?;
    ensure!(
        value["v"] == 1 && value["type"] == "registered",
        "relay registration rejected"
    );
    let online = value["peer_online"]
        .as_bool()
        .context("missing peer_online")?;
    Ok((ws, online))
}
pub async fn send_json(ws: &mut Socket, v: &impl serde::Serialize) -> Result<()> {
    let text = serde_json::to_string(v)?;
    ensure!(text.len() <= MAX_FRAME, "frame limit");
    timeout(Duration::from_secs(10), ws.send(Message::Text(text.into()))).await??;
    Ok(())
}
pub async fn receive_json(ws: &mut Socket) -> Result<Value> {
    receive_json_seen(ws, &mut Instant::now()).await
}
/// Like `receive_json`, but stamps `seen` for every frame including ping/pong,
/// which is what proves the relay path is alive.
async fn receive_json_seen(ws: &mut Socket, seen: &mut Instant) -> Result<Value> {
    loop {
        let frame = ws.next().await;
        if matches!(frame, Some(Ok(_))) {
            *seen = Instant::now();
        }
        match frame {
            Some(Ok(Message::Text(text))) => {
                ensure!(text.len() <= MAX_FRAME, "frame limit");
                return Ok(serde_json::from_str(&text)?);
            }
            Some(Ok(Message::Ping(p))) => {
                ws.send(Message::Pong(p)).await?;
            }
            Some(Ok(Message::Pong(_))) => {}
            _ => bail!("relay disconnected or unsupported frame"),
        }
    }
}

pub async fn start(storage: Storage, cli: PathBuf) -> Result<()> {
    ensure!(
        cli.is_absolute() && cli.is_file(),
        "--riwork must name an existing absolute RiWork executable path"
    );
    let _exclusive = storage.lock("connector.lock")?;
    let rpc = Arc::new(Rpc::new(cli, storage.clone()));
    let mut running: HashMap<String, (watch::Sender<bool>, tokio::task::JoinHandle<()>)> =
        HashMap::new();
    let mut tick = interval(Duration::from_millis(250));
    eprintln!(
        "RiWork connector running; paired device config is watched; Ctrl-C stops transport only."
    );
    loop {
        tokio::select! {
            _=tokio::signal::ctrl_c()=>break,
            _=tick.tick()=>{
                let cfg=match storage.config(){Ok(c)=>c,Err(e)=>{
                    // Fail closed if protected config disappears/corrupts permissions.
                    for (_, (cancel,_)) in running.drain(){let _=cancel.send(true);}
                    return Err(e);
                }};
                let active:HashMap<_,_>=cfg.devices.into_iter().filter(|d|!d.revoked).map(|d|(d.pairing.device_id.clone(),d)).collect();
                let removed=running.keys().filter(|id|!active.contains_key(*id)).cloned().collect::<Vec<_>>();
                for id in removed {if let Some((cancel,_))=running.remove(&id){let _=cancel.send(true);eprintln!("Remote device {id} was removed or revoked; connection closed.");}}
                for (id,device) in active {
                    if let std::collections::hash_map::Entry::Vacant(entry) = running.entry(id) {
                        // A pairing made by any local process is adopted here within a tick.
                        eprintln!("Remote device {} ({}) enabled.",device.pairing.device_id,log_safe(&device.pairing.device_name));
                        let (cancel,rx)=watch::channel(false); let rpc=rpc.clone();
                        let handle=tokio::spawn(async move{supervise(device,rpc,rx).await;});
                        entry.insert((cancel,handle));
                    }
                }
            }
        }
    }
    for (_, (cancel, handle)) in running {
        let _ = cancel.send(true);
        let _ = timeout(Duration::from_secs(1), handle).await;
    }
    Ok(())
}
/// Repeats of one failure (relay down, rejected registration) log once a minute.
#[derive(Default)]
struct FailureLog {
    last: Option<(String, Instant)>,
    suppressed: u32,
}
impl FailureLog {
    fn note(&mut self, device: &str, error: &anyhow::Error) {
        let reason = log_safe(&format!("{error:#}"));
        if let Some((last, at)) = &self.last
            && *last == reason
            && at.elapsed() < Duration::from_secs(60)
        {
            self.suppressed += 1;
            return;
        }
        let more = match self.suppressed {
            0 => String::new(),
            n => format!(" ({n} identical failures not logged)"),
        };
        eprintln!(
            "Remote device {device} disconnected: {reason}{more}; reconnecting (no payload logged)."
        );
        self.last = Some((reason, Instant::now()));
        self.suppressed = 0;
    }
}
async fn supervise(device: Device, rpc: Arc<Rpc>, mut cancel: watch::Receiver<bool>) {
    let mut failures = FailureLog::default();
    loop {
        if *cancel.borrow() {
            return;
        }
        // Cancelling a pending input drops the CLI child but keeps the durable
        // pending ledger. Retry yields outcome_unknown instead of duplicate input.
        tokio::select! {
            _=cancel.changed()=>return,
            result=run_device(&device,&rpc)=>{
                if let Err(e)=result{failures.note(&device.pairing.device_id,&e);}
            }
        }
        tokio::select! {_=cancel.changed()=>return,_=sleep(Duration::from_secs(1))=>{}}
    }
}
/// Liveness and lease timing; tests shorten these.
#[derive(Clone, Copy)]
pub(crate) struct Timing {
    ping: Duration,
    /// Nothing from the relay (its pings and pongs count) for this long means the
    /// path is dead, e.g. after a network switch or sleep.
    idle: Duration,
    renew: Duration,
    /// The viewport lease is renewed only while the phone was heard from this
    /// recently, so a vanished phone stops holding the desktop terminal at its size.
    mobile_active: Duration,
    /// While requests are pending, how often the device is checked for
    /// revocation. Without pending requests the connector's own device watch
    /// (every 250 ms) already ends a revoked device's connection.
    revoke_poll: Duration,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            ping: Duration::from_secs(20),
            idle: Duration::from_secs(60),
            renew: Duration::from_secs(3),
            mobile_active: Duration::from_secs(20),
            revoke_poll: Duration::from_millis(250),
        }
    }
}

/// The viewport of one session, shared by the requests running for it. `None`
/// once the session is over, so a straggler cannot resize for a session that
/// no longer exists.
type SharedViewport = Arc<tokio::sync::Mutex<Option<Viewport>>>;

/// A received request waiting for a slot; see `lanes`.
struct Queued {
    /// The session it arrived in.
    epoch: u64,
    viewport: SharedViewport,
    request: Value,
    /// When it was decrypted: the start of the reply's `server_ms`.
    received: std::time::Instant,
    /// The most JSON its response may hold (see `link`).
    reply_limit: usize,
}

/// How a request ended. `outcome` is `None` when it was cut short because its
/// session ended; an `Err` closes the connection, as it always did.
struct Done {
    lane: Lane,
    epoch: u64,
    received: std::time::Instant,
    outcome: Option<Result<Value>>,
}

/// Starts every queued request that has a slot, each as a task of its own so
/// that a slow one never keeps the connection loop, or another request, waiting.
/// `session_ended` carries the number of the current session: a request of an
/// earlier one that may be cut short (`Lane::cancellable`) is dropped when
/// it changes, and with it its CLI process.
fn start_ready(
    lanes: &mut Lanes<Queued>,
    tasks: &mut JoinSet<Done>,
    rpc: &Arc<Rpc>,
    device: &str,
    session_ended: &watch::Sender<u64>,
) {
    while let Some((lane, queued)) = lanes.next_ready() {
        let (rpc, device, mut ended) = (rpc.clone(), device.to_owned(), session_ended.subscribe());
        tasks.spawn(async move {
            let Queued {
                epoch,
                viewport,
                request,
                received,
                reply_limit,
            } = queued;
            let run = rpc.handle_shared_up_to(&device, request, &viewport, reply_limit);
            let outcome = if lane.cancellable() {
                tokio::select! {
                    result = run => Some(result),
                    _ = ended.wait_for(|current| *current != epoch) => None,
                }
            } else {
                Some(run.await)
            };
            Done {
                lane,
                epoch,
                received,
                outcome,
            }
        });
    }
}

/// Records the handshake off the async threads; a failure only costs the audit trail.
fn record_authentication(storage: Storage, id: String, name: String) {
    tokio::task::spawn_blocking(move || {
        let name = log_safe(&name);
        match storage.record_authentication(&id, Duration::from_secs(2)) {
            Ok(true) => eprintln!("Remote device {id} ({name}) authenticated for the first time."),
            Ok(false) => eprintln!("Remote device {id} ({name}) authenticated."),
            Err(e) => eprintln!(
                "Remote device {id} ({name}) authenticated; recording it failed: {}",
                log_safe(&format!("{e:#}"))
            ),
        }
    });
}
/// Seals a response: `server_ms` added, compressed if the phone asked for that and
/// it pays, and, if it cannot fit one frame either way, replaced by a
/// `response_too_large` error for the same request (the phone asks for less).
async fn seal_reply(
    session: &mut Session,
    response: Value,
    received: std::time::Instant,
    compress: bool,
) -> Result<crate::crypto::Envelope> {
    let body = serde_json::to_vec(&response)?;
    // A big body is deflated on a blocking thread so the connection loop (the
    // heartbeat, the socket, the other requests) is not held for the duration.
    let encoded = if compress && body.len() >= link::OFFLOAD_BYTES {
        tokio::task::spawn_blocking(move || {
            link::encode_body(body, received, compress, MAX_PLAINTEXT)
        })
        .await?
    } else {
        link::encode_body(body, received, compress, MAX_PLAINTEXT)
    };
    let plaintext = match encoded {
        Ok(encoded) => encoded.plaintext,
        Err(EncodeError::TooLarge) => {
            // Same request, small answer; a request without a usable id gets `null` as ever.
            let id = response.get("id").cloned().unwrap_or(Value::Null);
            let error = crate::rpc::error_for(
                id,
                "response_too_large",
                "result exceeds encrypted response limit; reduce output lines",
            );
            link::encode_reply(&error, received, false, MAX_PLAINTEXT)
                .map_err(anyhow::Error::from)?
                .plaintext
        }
        Err(EncodeError::Other(e)) => return Err(e),
    };
    session.seal("d2c", &plaintext)
}
async fn run_device(device: &Device, rpc: &Arc<Rpc>) -> Result<()> {
    run_device_with(device, rpc, Timing::default()).await
}
/// One connection of one device, until it fails or is dropped.
///
/// Requests are read, decrypted and answered by this one loop, but they are
/// carried out concurrently (see `lanes`) so that a `shell.output` that waits
/// for a change never delays typing or a resize. Responses go out in the order
/// they finish, each matched to its request by id. The loop alone seals and
/// sends frames, so their counters stay in order. Everything a connection
/// started ends with it: the tasks are aborted when the loop returns or is
/// cancelled, which kills their CLI processes.
pub(crate) async fn run_device_with(device: &Device, rpc: &Arc<Rpc>, timing: Timing) -> Result<()> {
    // Reload so a v2 invite redeemed on the previous connection is not stale.
    let device = rpc
        .storage
        .fresh_device(&device.pairing.device_id)?
        .context("revoked device")?;
    let p = &device.pairing;
    p.validate(device.allow_insecure_loopback)?;
    let (mut ws, online) =
        connect_registered(&p.relay_url, &p.route_id, "desktop", &device.desktop_token).await?;
    let identity = p.identity();
    let v1_secret = if p.v == 1 {
        Some(decode::<32>(&p.pairing_secret)?)
    } else {
        None
    };
    let mut root = match p.root_key.as_deref() {
        Some(value) => Some(decode::<32>(value)?),
        None => None,
    };
    let mut pending: Option<Pending> = None;
    let mut pair_state: Option<(Vec<u8>, [u8; 32])> = None;
    let mut session: Option<Session> = None;
    // Whether this session's replies may be compressed: the phone asked with
    // `link.configure` (see `link`). Every new session starts without.
    let mut compress = false;
    let mut viewport: Option<SharedViewport> = None;
    // Request tasks; dropped (so aborted) with this function, however it ends.
    let mut tasks: JoinSet<Done> = JoinSet::new();
    let mut lanes: Lanes<Queued> = Lanes::default();
    // Counts sessions: bumped when one ends, so that what its requests still
    // produce is neither sent nor waited for.
    let (session_ended, _) = watch::channel(0u64);
    let mut epoch = 0u64;
    let mut deadline: Option<Instant> =
        online.then(|| Instant::now() + Duration::from_secs(HANDSHAKE_SECONDS));
    let mut heartbeat = interval(timing.ping);
    let mut renew = interval(timing.renew);
    let mut revocation = interval(timing.revoke_poll);
    revocation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await;
    let mut last_rx = Instant::now();
    let mut last_mobile = Instant::now();
    loop {
        let idle_at = last_rx + timing.idle;
        // Reading pauses only when a device has queued more than it should.
        let reading = lanes.queued() < MAX_QUEUED;
        // Biased: frames already waiting after a slow RPC must be read before the
        // idle deadline is judged. Ticks come first so a busy peer cannot starve
        // them, then finished requests, so answers do not wait behind new ones.
        let value = tokio::select! {
            biased;
            _=renew.tick()=>{
                // A resize that is running renews the lease itself.
                if let Some(shared)=&viewport
                    && last_mobile.elapsed()<=timing.mobile_active
                    && let Ok(held)=shared.try_lock()
                    && let Some(v)=held.as_ref()
                {
                    rpc.renew_viewport(v).await?;
                }
                continue;
            },
            _=heartbeat.tick()=>{timeout(Duration::from_secs(10),ws.send(Message::Ping(vec![].into()))).await??;continue;},
            _=revocation.tick(), if !tasks.is_empty() || lanes.queued()>0 =>{
                // A long wait must not outlive the device's authorization.
                ensure!(rpc.storage.authorized(&p.device_id)?, "device revoked during RPC");
                continue;
            },
            Some(joined)=tasks.join_next(), if !tasks.is_empty() =>{
                let done=joined.context("request task failed")?;
                lanes.finished(done.lane);
                start_ready(&mut lanes,&mut tasks,rpc,&p.device_id,&session_ended);
                if done.epoch==epoch && let Some(outcome)=done.outcome {
                    let response=outcome?;
                    ensure!(
                        rpc.storage.authorized(&p.device_id)?,
                        "device revoked during RPC"
                    );
                    let s=session.as_mut().context("response without a session")?;
                    let reply=seal_reply(s,response,done.received,compress).await?;
                    send_json(&mut ws,&reply).await?;
                }
                continue;
            },
            value=receive_json_seen(&mut ws,&mut last_rx), if reading =>value?,
            // Not while reading is paused: that silence is our doing, not the relay's.
            _=tokio::time::sleep_until(idle_at), if reading =>{
                // The receive branch above may have consumed a pong in this very poll.
                if last_rx.elapsed()>=timing.idle {bail!("relay silent past the liveness deadline");}
                continue;
            },
            _=async {if let Some(t)=deadline {tokio::time::sleep_until(t).await;}else{std::future::pending::<()>().await;}}=>bail!("handshake timeout"),
        };
        let ver = value["v"].as_u64();
        match value["type"].as_str() {
            Some("peer") => {
                ensure!(ver == Some(1), "unsupported version");
                let online = value["online"].as_bool().context("missing peer status")?;
                // Ordered peer control messages reset old transport state. What the
                // old session still asks for and can be dropped is: waits end, their
                // CLI processes with them. Typing and resizing finish; nobody is
                // left to hear about it.
                epoch += 1;
                session_ended.send_replace(epoch);
                lanes.drop_queued_cancellable();
                if let Some(shared) = viewport.take() {
                    // Waits for a resize that is running; the tasks run on their own.
                    if let Some(mut v) = shared.lock().await.take() {
                        rpc.clear_viewport(&mut v).await?;
                    }
                }
                session = None;
                compress = false;
                pending = None;
                pair_state = None;
                deadline = online.then(|| Instant::now() + Duration::from_secs(HANDSHAKE_SECONDS));
            }
            Some("pair_hello") => {
                ensure!(p.v == 2 && ver == Some(2), "unsupported version");
                ensure!(
                    session.is_none() && pending.is_none() && pair_state.is_none(),
                    "unexpected handshake restart"
                );
                ensure!(rpc.storage.authorized(&p.device_id)?, "revoked device");
                let hello: PairHello = serde_json::from_value(value)?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_secs());
                match rpc.storage.redeem_invite(&hello, now, random32()) {
                    Ok(done) => {
                        send_json(&mut ws, &done.accept).await?;
                        root = Some(done.root_key);
                        pair_state = Some((done.transcript, done.root_key));
                        deadline = Some(Instant::now() + Duration::from_secs(HANDSHAKE_SECONDS));
                    }
                    Err(crate::config::RedeemError::Rejected(code)) => {
                        let _ =
                            send_json(&mut ws, &json!({"v":2,"type":"pair_error","error":code}))
                                .await;
                        bail!("pairing {code}");
                    }
                    Err(crate::config::RedeemError::Io(error)) => return Err(error),
                }
            }
            Some("pair_finish") => {
                ensure!(p.v == 2 && ver == Some(2), "unsupported version");
                let (transcript, key) = pair_state.take().context("unexpected pair finish")?;
                let finish: PairFinish = serde_json::from_value(value)?;
                crate::crypto::verify_pair_finish(&key, &transcript, &finish)?;
                deadline = Some(Instant::now() + Duration::from_secs(HANDSHAKE_SECONDS));
            }
            Some("client_hello") => {
                ensure!(
                    session.is_none() && pending.is_none() && pair_state.is_none(),
                    "unexpected handshake restart"
                );
                ensure!(rpc.storage.authorized(&p.device_id)?, "revoked device");
                if p.v == 1 {
                    ensure!(ver == Some(1), "unsupported version");
                    let secret = v1_secret.context("missing pairing secret")?;
                    let hello: ClientHello = serde_json::from_value(value)?;
                    let (reply, state) = accept_hello(&identity, &secret, &hello, random32())?;
                    send_json(&mut ws, &reply).await?;
                    pending = Some(state);
                } else if p.v == 2 {
                    ensure!(ver == Some(2), "unsupported version");
                    let key = root.context("pairing not established")?;
                    let hello: ClientHelloV2 = serde_json::from_value(value)?;
                    let (reply, state) =
                        accept_client_hello_v2(&identity, &key, &hello, random32())?;
                    send_json(&mut ws, &reply).await?;
                    pending = Some(state);
                } else {
                    bail!("unsupported version");
                }
                deadline = Some(Instant::now() + Duration::from_secs(HANDSHAKE_SECONDS));
            }
            Some("client_finish") => {
                let f: ClientFinish = serde_json::from_value(value)?;
                let mut s = pending.take().context("unexpected finish")?.finish(&f)?;
                // `features` is additive: an older phone reads only the three fields it knows.
                let ready = json!({"v":1,"type":"ready","desktop_id":p.desktop_id,"device_id":p.device_id,"features":link::features(rpc.history_max_lines())});
                let e = s.seal("d2c", &serde_json::to_vec(&ready)?)?;
                send_json(&mut ws, &e).await?;
                session = Some(s);
                compress = false;
                viewport = Some(Arc::new(tokio::sync::Mutex::new(Some(Viewport::new(
                    rpc.cli.clone(),
                    p.device_id.clone(),
                )))));
                deadline = None;
                last_mobile = Instant::now();
                record_authentication(
                    rpc.storage.clone(),
                    p.device_id.clone(),
                    p.device_name.clone(),
                );
            }
            Some("encrypted") => {
                let s = session
                    .as_mut()
                    .context("RPC before authenticated handshake")?;
                let envelope: Envelope = serde_json::from_value(value)?;
                let plaintext = s.open("c2d", &envelope)?;
                let received = std::time::Instant::now();
                last_mobile = Instant::now();
                // Unparseable plaintext is answered as an invalid request, not a dropped session.
                let request: Value = serde_json::from_slice(&plaintext).unwrap_or(Value::Null);
                if request.get("method").and_then(Value::as_str) == Some("link.configure") {
                    // About this connection, not the desktop: answered here, in order.
                    ensure!(
                        rpc.storage.authorized(&p.device_id)?,
                        "device revoked during RPC"
                    );
                    let (answer, change) = link::configure(&request, compress);
                    if let Some(on) = change {
                        compress = on;
                    }
                    let reply = seal_reply(s, answer, received, compress).await?;
                    send_json(&mut ws, &reply).await?;
                    continue;
                }
                lanes.push(
                    classify(&request),
                    Queued {
                        epoch,
                        viewport: viewport
                            .clone()
                            .context("RPC before authenticated handshake")?,
                        request,
                        received,
                        reply_limit: if compress {
                            link::MAX_INFLATED
                        } else {
                            MAX_PLAINTEXT
                        },
                    },
                );
                start_ready(&mut lanes, &mut tasks, rpc, &p.device_id, &session_ended);
            }
            _ => bail!("unexpected endpoint frame"),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::crypto::b64;
    use crate::{
        config::{Pairing, private_read},
        crypto::{ServerHello, accept_server, client_hello},
        relay::{Relay, Routes},
    };
    use std::path::Path;

    /// Stand-in RiWork CLI: logs each call and reports one live shell.
    fn scripted_cli(dir: &Path, shell: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let (cli, log) = (dir.join("fake-riwork"), dir.join("cli.log"));
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1 $2\" in\n\
                 'shell list') printf '[{{\"id\":\"{shell}\",\"alive\":true}}]';;\n\
                 'orchestrator list'|'project list') echo '[]';;\nesac\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        (cli, log)
    }
    /// Polls a condition instead of trusting a fixed sleep on a loaded machine.
    async fn eventually(mut condition: impl FnMut() -> bool) {
        for _ in 0..250 {
            if condition() {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
        panic!("condition not reached within 5 seconds");
    }
    fn renewals(log: &Path) -> usize {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("shell resize "))
            .count()
    }
    async fn paired(dir: &Path, relay_url: &str) -> (Storage, Device, PathBuf) {
        let storage = Storage::at(dir.to_path_buf()).unwrap();
        let routes = dir.join("routes.json");
        storage
            .pair(
                relay_url.into(),
                "phone".into(),
                true,
                &dir.join("pairing.json"),
                Some(&routes),
            )
            .unwrap();
        let device = storage.config().unwrap().devices.remove(0);
        (storage, device, routes)
    }
    async fn mobile(p: &Pairing) -> (Socket, Session) {
        let (mut ws, online) = loop {
            match connect_registered(&p.relay_url, &p.route_id, "mobile", &p.relay_token).await {
                Ok(s) => break s,
                Err(_) => sleep(Duration::from_millis(20)).await,
            }
        };
        if !online {
            let peer = receive_json(&mut ws).await.unwrap();
            assert_eq!(peer["online"], true);
        }
        let (nonce, secret) = (random32(), decode::<32>(&p.pairing_secret).unwrap());
        send_json(
            &mut ws,
            &client_hello(&p.identity(), &secret, nonce).unwrap(),
        )
        .await
        .unwrap();
        let hello: ServerHello =
            serde_json::from_value(receive_json(&mut ws).await.unwrap()).unwrap();
        let (finish, mut session) = accept_server(&p.identity(), &secret, &nonce, &hello).unwrap();
        send_json(&mut ws, &finish).await.unwrap();
        let ready: Envelope = serde_json::from_value(receive_json(&mut ws).await.unwrap()).unwrap();
        session.open("d2c", &ready).unwrap();
        (ws, session)
    }
    async fn call(ws: &mut Socket, s: &mut Session, method: &str, params: Value) -> Value {
        let request = json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":method,"params":params});
        send_json(
            ws,
            &s.seal("c2d", &serde_json::to_vec(&request).unwrap())
                .unwrap(),
        )
        .await
        .unwrap();
        let reply = timeout(Duration::from_secs(15), receive_json(ws))
            .await
            .expect("connector answers")
            .unwrap();
        let reply: Envelope = serde_json::from_value(reply).unwrap();
        serde_json::from_slice(&s.open("d2c", &reply).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn lease_renewal_needs_recent_phone_traffic_and_auth_is_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
        let (storage, device, routes) = paired(tmp.path(), &url).await;
        let relay = Relay::new(private_read::<Routes>(&routes, 1 << 20).unwrap(), 8).unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, relay.router()).await });
        let shell = uuid::Uuid::new_v4().to_string();
        let (cli, log) = scripted_cli(tmp.path(), &shell);
        let rpc = Arc::new(Rpc::new(cli, storage.clone()));
        // Short enough to test. Pings every 100 ms against a 1 s idle deadline also
        // prove a healthy but quiet connection is not mistaken for a dead one.
        let timing = Timing {
            ping: Duration::from_millis(100),
            idle: Duration::from_secs(1),
            renew: Duration::from_millis(50),
            mobile_active: Duration::from_secs(1),
            revoke_poll: Duration::from_millis(50),
        };
        let pairing = device.pairing.clone();
        let connector = tokio::spawn(async move { run_device_with(&device, &rpc, timing).await });

        let (mut ws, mut session) = mobile(&pairing).await;
        let resized = call(
            &mut ws,
            &mut session,
            "shell.resize",
            json!({"shell_id":shell,"columns":43,"rows":17}),
        )
        .await;
        assert_eq!(resized["ok"], true, "{resized}");
        // While the phone keeps talking, the lease is renewed. (Traffic is repeated
        // so a slow machine cannot outrun the 1 s window before renewals happen.)
        for _ in 0..100 {
            if renewals(&log) >= 3 {
                break;
            }
            let listed = call(&mut ws, &mut session, "projects.list", json!({})).await;
            assert_eq!(listed["ok"], true, "{listed}");
            sleep(Duration::from_millis(50)).await;
        }
        assert!(renewals(&log) >= 3, "renews while the phone is active");
        for _ in 0..50 {
            if storage.config().unwrap().devices[0]
                .last_authenticated_unix
                .is_some()
            {
                break;
            }
            sleep(Duration::from_millis(40)).await;
        }
        let recorded = storage.config().unwrap().devices.remove(0);
        assert!(
            recorded.first_authenticated_unix.is_some()
                && recorded.last_authenticated_unix.is_some()
        );

        // The phone goes quiet (still connected at transport level): renewals stop.
        sleep(Duration::from_millis(3000)).await;
        let stopped = renewals(&log);
        sleep(Duration::from_millis(1000)).await;
        assert_eq!(renewals(&log), stopped, "no renewal without phone traffic");
        assert!(
            !connector.is_finished(),
            "a quiet, healthy connection stays up"
        );

        // Any authenticated request revives it.
        let listed = call(&mut ws, &mut session, "projects.list", json!({})).await;
        assert_eq!(listed["ok"], true, "{listed}");
        eventually(|| renewals(&log) > stopped).await;
        connector.abort();
        server.abort();
    }

    #[tokio::test]
    async fn connector_gives_up_on_a_relay_that_stops_answering() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
        // Registers the connector, then black-holes: never reads, pongs or writes.
        let hole = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let _register = ws.next().await;
            let ack = r#"{"v":1,"type":"registered","peer_online":false}"#;
            ws.send(Message::Text(ack.into())).await.unwrap();
            std::future::pending::<()>().await;
        });
        let tmp = tempfile::tempdir().unwrap();
        let (storage, device, _) = paired(tmp.path(), &url).await;
        let (cli, _) = scripted_cli(tmp.path(), &uuid::Uuid::new_v4().to_string());
        let rpc = Arc::new(Rpc::new(cli, storage));
        let timing = Timing {
            ping: Duration::from_millis(100),
            idle: Duration::from_millis(400),
            ..Timing::default()
        };
        let started = Instant::now();
        let outcome = timeout(
            Duration::from_secs(5),
            run_device_with(&device, &rpc, timing),
        )
        .await
        .expect("must not wait forever on a silent relay");
        let error = format!("{:#}", outcome.unwrap_err());
        assert!(error.contains("liveness deadline"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
        hole.abort();
    }

    #[tokio::test]
    async fn v2_relay_rejects_race_replay_and_old_hello_then_continues() {
        use crate::crypto::pair_hello;
        let tmp = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
        let storage = Storage::at(tmp.path().to_path_buf()).unwrap();
        let routes = tmp.path().join("routes.json");
        let export = storage
            .pair_with(
                url.clone(),
                "phone".into(),
                true,
                &tmp.path().join("phone.json"),
                Some(&routes),
                2,
                600,
            )
            .unwrap();
        let invite_secret = decode::<32>(export.invite_secret.as_deref().unwrap()).unwrap();
        let relay = Relay::new(private_read::<Routes>(&routes, 1 << 20).unwrap(), 8).unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, relay.router()).await });
        let (cli, _) = scripted_cli(tmp.path(), &uuid::Uuid::new_v4().to_string());
        let rpc = Arc::new(Rpc::new(cli, storage.clone()));
        let hold = storage.testing_hold_invite(export.invite_id.as_deref().unwrap());
        let device = storage.config().unwrap().devices.remove(0);
        let connector =
            tokio::spawn(async move { run_device_with(&device, &rpc, Timing::default()).await });
        let mut raced = connect_mobile(&export).await;
        let hello = pair_hello(
            &export.identity(),
            &export.relay_url,
            export.invite_id.as_deref().unwrap(),
            export.expires_at.unwrap(),
            &invite_secret,
            [11u8; 32],
        )
        .unwrap();
        send_json(&mut raced, &hello).await.unwrap();
        let error = receive_json(&mut raced).await.unwrap();
        assert_eq!(error["error"], "invite_race");
        let reason = format!("{:#}", connector.await.unwrap().unwrap_err());
        assert!(reason.contains("invite_race"), "{reason}");
        drop(raced);
        drop(hold);
        assert_eq!(
            storage.config().unwrap().devices[0]
                .pairing
                .invite_state
                .as_deref(),
            Some("pending")
        );

        let device = storage.fresh_device(&export.device_id).unwrap().unwrap();
        let rpc = Arc::new(Rpc::new(
            scripted_cli(tmp.path(), &uuid::Uuid::new_v4().to_string()).0,
            storage.clone(),
        ));
        let connector =
            tokio::spawn(async move { run_device_with(&device, &rpc, Timing::default()).await });
        let mut ws = connect_mobile(&export).await;
        // A v1 hello cannot open a v2 device.
        send_json(&mut ws, &json!({"v":1,"type":"client_hello","desktop_id":export.desktop_id,"device_id":export.device_id,"route_id":export.route_id,"client_nonce":"AA","mac":"AA"})).await.unwrap();
        // The desktop closes. The relay may deliver peer-offline before the socket ends.
        let next = timeout(Duration::from_secs(2), receive_json(&mut ws))
            .await
            .unwrap();
        if let Ok(value) = next {
            assert_ne!(value["type"], "server_hello", "{value}");
        }
        drop(ws);
        let reason = format!("{:#}", connector.await.unwrap().unwrap_err());
        assert!(reason.contains("unsupported version"), "{reason}");

        let device = storage.fresh_device(&export.device_id).unwrap().unwrap();
        let rpc = Arc::new(Rpc::new(
            scripted_cli(tmp.path(), &uuid::Uuid::new_v4().to_string()).0,
            storage.clone(),
        ));
        let connector =
            tokio::spawn(async move { run_device_with(&device, &rpc, Timing::default()).await });
        let (mut ws, mut session, root) = establish_v2(&export, &invite_secret).await;
        let listed = call(&mut ws, &mut session, "projects.list", json!({})).await;
        assert_eq!(listed["ok"], true);
        assert_eq!(listed["result"]["projects"], json!([]));
        let first_id = session.id;
        ws.close(None).await.unwrap();
        drop(ws);
        let (mut ws, mut again) = resume_v2(&export, &root).await;
        assert_ne!(again.id, first_id);
        assert_eq!(
            call(&mut ws, &mut again, "projects.list", json!({})).await["ok"],
            true
        );
        ws.close(None).await.unwrap();
        drop(ws);
        let mut replay = connect_mobile(&export).await;
        send_json(&mut replay, &hello).await.unwrap();
        let error = receive_json(&mut replay).await.unwrap();
        assert_eq!(error["error"], "invite_replay");
        let reason = format!("{:#}", connector.await.unwrap().unwrap_err());
        assert!(reason.contains("invite_replay"), "{reason}");
        let saved = std::fs::read_to_string(storage.dir.join("devices.json")).unwrap();
        let routes_raw = std::fs::read_to_string(&routes).unwrap();
        assert!(!saved.contains(export.invite_secret.as_deref().unwrap()));
        assert!(!routes_raw.contains(export.invite_secret.as_deref().unwrap()));
        assert!(!routes_raw.contains("projects.list"));
        server.abort();
    }

    async fn connect_mobile(p: &Pairing) -> Socket {
        let (mut ws, online) = loop {
            match connect_registered(&p.relay_url, &p.route_id, "mobile", &p.relay_token).await {
                Ok(ready) => break ready,
                Err(_) => sleep(Duration::from_millis(20)).await,
            }
        };
        if !online {
            let peer = receive_json(&mut ws).await.unwrap();
            assert_eq!(peer["online"], true);
        }
        ws
    }
    async fn establish_v2(p: &Pairing, invite_secret: &[u8; 32]) -> (Socket, Session, [u8; 32]) {
        use crate::crypto::{accept_pair, accept_server_hello_v2, client_hello_v2, pair_hello};
        let mut ws = connect_mobile(p).await;
        let nonce = random32();
        let hello = pair_hello(
            &p.identity(),
            &p.relay_url,
            p.invite_id.as_deref().unwrap(),
            p.expires_at.unwrap(),
            invite_secret,
            nonce,
        )
        .unwrap();
        let wire = serde_json::to_string(&hello).unwrap();
        assert!(!wire.contains(&b64(invite_secret)));
        send_json(&mut ws, &hello).await.unwrap();
        let accept: crate::crypto::PairAccept =
            serde_json::from_value(receive_json(&mut ws).await.unwrap()).unwrap();
        let (finish, root, _) = accept_pair(
            &p.identity(),
            &p.relay_url,
            p.invite_id.as_deref().unwrap(),
            p.expires_at.unwrap(),
            invite_secret,
            &nonce,
            &accept,
        )
        .unwrap();
        send_json(&mut ws, &finish).await.unwrap();
        // client_hello_v2 wipes only its copy of the scalar. The caller keeps this one for finish.
        let client_private = random32();
        let (session_hello, _) = client_hello_v2(&p.identity(), &root, client_private).unwrap();
        send_json(&mut ws, &session_hello).await.unwrap();
        let server: crate::crypto::ServerHelloV2 =
            serde_json::from_value(receive_json(&mut ws).await.unwrap()).unwrap();
        let (finish, mut session) =
            accept_server_hello_v2(&p.identity(), &root, client_private, &server).unwrap();
        send_json(&mut ws, &finish).await.unwrap();
        let ready: Envelope = serde_json::from_value(receive_json(&mut ws).await.unwrap()).unwrap();
        session.open("d2c", &ready).unwrap();
        (ws, session, root)
    }
    async fn resume_v2(p: &Pairing, root: &[u8; 32]) -> (Socket, Session) {
        use crate::crypto::{accept_server_hello_v2, client_hello_v2};
        let mut ws = connect_mobile(p).await;
        let client_private = random32();
        let (hello, _) = client_hello_v2(&p.identity(), root, client_private).unwrap();
        send_json(&mut ws, &hello).await.unwrap();
        let server: crate::crypto::ServerHelloV2 =
            serde_json::from_value(receive_json(&mut ws).await.unwrap()).unwrap();
        let (finish, mut session) =
            accept_server_hello_v2(&p.identity(), root, client_private, &server).unwrap();
        send_json(&mut ws, &finish).await.unwrap();
        let ready: Envelope = serde_json::from_value(receive_json(&mut ws).await.unwrap()).unwrap();
        session.open("d2c", &ready).unwrap();
        (ws, session)
    }

    fn stamp(params: &mut rcgen::CertificateParams) {
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::hours(1);
        params.not_after = now + time::Duration::hours(2);
    }

    /// Short-lived CA and a leaf whose only SAN is the IP 127.0.0.1.
    fn test_certificate() -> (
        tokio_rustls::TlsAcceptor,
        rustls::pki_types::CertificateDer<'static>,
    ) {
        install_ring_provider();
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "RiWork Test CA");
        ca_params
            .key_usages
            .push(rcgen::KeyUsagePurpose::DigitalSignature);
        ca_params
            .key_usages
            .push(rcgen::KeyUsagePurpose::KeyCertSign);
        ca_params.key_usages.push(rcgen::KeyUsagePurpose::CrlSign);
        stamp(&mut ca_params);
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let ca = ca_params.self_signed(&ca_key).unwrap();

        let mut leaf_params = rcgen::CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
        assert!(matches!(
            leaf_params.subject_alt_names.as_slice(),
            [rcgen::SanType::IpAddress(std::net::IpAddr::V4(ip))]
                if *ip == std::net::Ipv4Addr::LOCALHOST
        ));
        leaf_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "127.0.0.1");
        leaf_params.use_authority_key_identifier_extension = true;
        leaf_params
            .key_usages
            .push(rcgen::KeyUsagePurpose::DigitalSignature);
        leaf_params
            .extended_key_usages
            .push(rcgen::ExtendedKeyUsagePurpose::ServerAuth);
        stamp(&mut leaf_params);
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key).unwrap();
        let server = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(
                    rustls::pki_types::PrivatePkcs8KeyDer::from(leaf_key.serialize_der()),
                ),
            )
            .unwrap();
        (
            tokio_rustls::TlsAcceptor::from(Arc::new(server)),
            ca.der().clone(),
        )
    }

    fn trusting(ca: rustls::pki_types::CertificateDer<'static>) -> Arc<rustls::ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca).unwrap();
        Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    }

    /// Loopback TLS byte-pipe. Listens on IPv4 and IPv6 so `localhost` reaches it.
    async fn tls_proxy(
        acceptor: tokio_rustls::TlsAcceptor,
        relay: std::net::SocketAddr,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let v4 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = v4.local_addr().unwrap();
        let v6 = tokio::net::TcpListener::bind(format!("[::1]:{}", addr.port()))
            .await
            .unwrap();
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    result = v4.accept() => result,
                    result = v6.accept() => result,
                };
                let Ok((tcp, _)) = accepted else {
                    break;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let Ok(mut upstream) = tokio::net::TcpStream::connect(relay).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut tls, &mut upstream).await;
                });
            }
        });
        (addr, task)
    }

    fn expect_certificate_error(result: Result<(Socket, bool)>, label: &str, needles: &[&str]) {
        let text = match result {
            Ok(_) => panic!("{label} completed registration"),
            Err(err) => format!("{err:#}"),
        };
        assert!(
            !text.contains("CryptoProvider"),
            "{label} panicked in provider selection: {text}"
        );
        for needle in needles {
            assert!(text.contains(needle), "{label}: {text}");
        }
    }

    #[tokio::test]
    async fn wss_rejects_an_untrusted_cert_and_carries_v1_and_v2_when_trusted() {
        let relay_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay_listener.local_addr().unwrap();
        let (acceptor, ca) = test_certificate();
        let trusted = trusting(ca);
        let (front, front_task) = tls_proxy(acceptor, relay_addr).await;
        let url = format!("wss://{front}/v1/ws");
        let tmp = tempfile::tempdir().unwrap();
        let (storage, device, routes) = paired(tmp.path(), &url).await;
        let export = storage
            .pair_with(
                url.clone(),
                "phone-v2".into(),
                true,
                &tmp.path().join("phone-v2.json"),
                Some(&routes),
                2,
                600,
            )
            .unwrap();
        let invite_secret = decode::<32>(export.invite_secret.as_deref().unwrap()).unwrap();
        let relay = Relay::new(private_read::<Routes>(&routes, 1 << 20).unwrap(), 8).unwrap();
        let server = tokio::spawn(async move { axum::serve(relay_listener, relay.router()).await });

        expect_certificate_error(
            connect_registered(
                &device.pairing.relay_url,
                &device.pairing.route_id,
                "mobile",
                &device.pairing.relay_token,
            )
            .await,
            "untrusted certificate",
            &["invalid peer certificate", "UnknownIssuer"],
        );

        let _trust = TestWssTrust::install(trusted);
        // The leaf SAN is only the IP 127.0.0.1. `localhost` is a DNS name.
        expect_certificate_error(
            connect_registered(
                &format!("wss://localhost:{}/v1/ws", front.port()),
                &device.pairing.route_id,
                "mobile",
                &device.pairing.relay_token,
            )
            .await,
            "name mismatch",
            &["invalid peer certificate", "not valid for name"],
        );

        let pairing = device.pairing.clone();
        let (cli, _) = scripted_cli(tmp.path(), &uuid::Uuid::new_v4().to_string());
        let rpc = Arc::new(Rpc::new(cli, storage.clone()));
        let connector =
            tokio::spawn(async move { run_device_with(&device, &rpc, Timing::default()).await });
        let (mut ws, mut session) = timeout(Duration::from_secs(20), mobile(&pairing))
            .await
            .expect("v1 handshake over wss");
        let listed = call(&mut ws, &mut session, "projects.list", json!({})).await;
        assert_eq!(listed["ok"], true, "{listed}");
        assert_eq!(listed["result"]["projects"], json!([]));
        ws.close(None).await.unwrap();
        drop(ws);
        connector.abort();

        let device = storage.fresh_device(&export.device_id).unwrap().unwrap();
        let (cli, _) = scripted_cli(tmp.path(), &uuid::Uuid::new_v4().to_string());
        let rpc = Arc::new(Rpc::new(cli, storage.clone()));
        let connector =
            tokio::spawn(async move { run_device_with(&device, &rpc, Timing::default()).await });
        let (mut ws, mut session, root) = timeout(
            Duration::from_secs(20),
            establish_v2(&export, &invite_secret),
        )
        .await
        .expect("v2 invite over wss");
        let listed = call(&mut ws, &mut session, "projects.list", json!({})).await;
        assert_eq!(listed["ok"], true, "{listed}");
        assert_eq!(listed["result"]["projects"], json!([]));
        ws.close(None).await.unwrap();
        drop(ws);
        let (mut ws, mut again) = timeout(Duration::from_secs(20), resume_v2(&export, &root))
            .await
            .expect("v2 resume over wss");
        let listed = call(&mut ws, &mut again, "projects.list", json!({})).await;
        assert_eq!(listed["ok"], true, "{listed}");
        assert_eq!(listed["result"]["projects"], json!([]));
        ws.close(None).await.unwrap();
        let v2_id = export.device_id.clone();
        eventually(|| {
            storage.config().ok().is_some_and(|config| {
                config.devices.iter().any(|device| {
                    device.pairing.device_id == v2_id && device.last_authenticated_unix.is_some()
                })
            })
        })
        .await;
        // Authentication records are spawned off the handshake task. Let them
        // finish before the temporary home disappears.
        sleep(Duration::from_millis(200)).await;
        connector.abort();
        server.abort();
        front_task.abort();
    }

    // Concurrent requests of one device.

    /// A stand-in CLI whose `shell output` blocks until the file `gate` exists
    /// in its directory (and records its process ID), whose `shell keys`
    /// sleeps if `keys-slow` exists and blocks on `keys-gate` while
    /// `keys-block` exists, and whose `shell resize` is quick. Every call is
    /// logged in `cli.log`; the start and end of `shell keys` and `shell
    /// resize` in `order.log`. Answers are the CLI's `--json` forms. `shell
    /// history` blocks like `shell output` until the file `history-gate`
    /// exists.
    fn gated_cli(dir: &Path, shell: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let cli = dir.join("gated-riwork");
        let script = r#"#!/bin/sh
D='@DIR@'
S='@SHELL@'
printf '%s\n' "$*" >> "$D/cli.log"
wait_for() { i=0; while [ ! -e "$D/$1" ] && [ "$i" -lt 1000 ]; do sleep 0.02; i=$((i+1)); done; }
case "$1 $2" in
'shell list') printf '[{"id":"%s","alive":true}]' "$S";;
'orchestrator list'|'project list') echo '[]';;
'shell output')
  echo $$ >> "$D/output.pids"
  wait_for gate
  if [ -e "$D/unchanged" ]; then
    printf '{"id":"%s","unchanged":true,"hash":"0123456789abcdef"}' "$S"
  else
    printf '{"id":"%s","output":"hi\\n","hash":"0123456789abcdef"}' "$S"
  fi;;
'shell history')
  echo $$ >> "$D/history.pids"
  wait_for history-gate
  printf '{"id":"%s","output":"1\\n2","line_count":2,"history_size":10,"complete":false}' "$S";;
'shell keys')
  echo "begin keys $5" >> "$D/order.log"
  if [ -e "$D/keys-slow" ]; then sleep 0.3; fi
  if [ -e "$D/keys-block" ]; then wait_for keys-gate; fi
  echo "end keys $5" >> "$D/order.log";;
'shell resize')
  echo "begin resize" >> "$D/order.log"
  sleep 0.1
  echo "end resize" >> "$D/order.log";;
esac
"#
        .replace("@DIR@", &dir.to_string_lossy())
        .replace("@SHELL@", shell);
        std::fs::write(&cli, script).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        cli
    }
    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), "").unwrap();
    }
    fn logged(dir: &Path, name: &str, prefix: &str) -> usize {
        std::fs::read_to_string(dir.join(name))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with(prefix))
            .count()
    }
    fn alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// One connector on a real relay, served by `gated_cli`.
    struct Rig {
        _tmp: tempfile::TempDir,
        dir: PathBuf,
        storage: Storage,
        pairing: Pairing,
        shell: String,
        connector: tokio::task::JoinHandle<Result<()>>,
        server: tokio::task::JoinHandle<std::io::Result<()>>,
    }
    impl Rig {
        async fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
            let (storage, device, routes) = paired(tmp.path(), &url).await;
            let relay = Relay::new(private_read::<Routes>(&routes, 1 << 20).unwrap(), 8).unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, relay.router()).await });
            let shell = uuid::Uuid::new_v4().to_string();
            let dir = tmp.path().to_path_buf();
            let rpc = Arc::new(Rpc::new(gated_cli(&dir, &shell), storage.clone()));
            let timing = Timing {
                ping: Duration::from_secs(20),
                idle: Duration::from_secs(60),
                // No lease renewals in the middle of an ordering test.
                renew: Duration::from_secs(300),
                mobile_active: Duration::from_secs(20),
                revoke_poll: Duration::from_millis(50),
            };
            let pairing = device.pairing.clone();
            let connector =
                tokio::spawn(async move { run_device_with(&device, &rpc, timing).await });
            Self {
                _tmp: tmp,
                dir,
                storage,
                pairing,
                shell,
                connector,
                server,
            }
        }
        fn count(&self, prefix: &str) -> usize {
            logged(&self.dir, "cli.log", prefix)
        }
        fn output_calls(&self) -> usize {
            self.count("shell output")
        }
        fn output_pids(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.join("output.pids"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }
        fn output_request(&self, params: Value) -> Value {
            let mut params = params;
            params["shell_id"] = json!(self.shell);
            params
        }
    }
    impl Drop for Rig {
        fn drop(&mut self) {
            self.connector.abort();
            self.server.abort();
        }
    }
    fn wait_params(shell: &str, wait_ms: u64) -> Value {
        json!({"shell_id":shell,"if_changed":"0123456789abcdef","wait_ms":wait_ms})
    }

    async fn send_request(ws: &mut Socket, s: &mut Session, method: &str, params: Value) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let request = json!({"v":1,"type":"request","id":id,"method":method,"params":params});
        send_json(
            ws,
            &s.seal("c2d", &serde_json::to_vec(&request).unwrap())
                .unwrap(),
        )
        .await
        .unwrap();
        id
    }
    /// The next frame of the connector. Opening it proves it carries the next
    /// counter, so a run of these also proves frames left in counter order.
    async fn next_response(ws: &mut Socket, s: &mut Session) -> Value {
        let frame = timeout(Duration::from_secs(15), receive_json(ws))
            .await
            .expect("connector answers")
            .unwrap();
        let envelope: Envelope = serde_json::from_value(frame).unwrap();
        serde_json::from_slice(&s.open("d2c", &envelope).unwrap()).unwrap()
    }
    async fn expect_response(ws: &mut Socket, s: &mut Session, id: &str) -> Value {
        let response = next_response(ws, s).await;
        assert_eq!(response["id"], id, "{response}");
        response
    }
    fn keys_params(shell: &str, text: &str) -> Value {
        json!({"shell_id":shell,"batch":uuid::Uuid::new_v4().to_string(),"items":[{"text":text}]})
    }

    #[tokio::test]
    async fn typing_resizing_and_reads_are_answered_while_an_output_wait_is_pending() {
        let rig = Rig::new().await;
        let (mut ws, mut s) = mobile(&rig.pairing).await;
        let wait = send_request(
            &mut ws,
            &mut s,
            "shell.output",
            wait_params(&rig.shell, 10_000),
        )
        .await;
        eventually(|| rig.output_calls() == 1).await;

        // The wait is stuck in its CLI; everything else still gets through, and
        // the answers are the next frames, so none was queued behind the wait.
        let keys = send_request(
            &mut ws,
            &mut s,
            "shell.keys",
            keys_params(&rig.shell, "typed"),
        )
        .await;
        let reply = expect_response(&mut ws, &mut s, &keys).await;
        assert_eq!(reply["result"]["status"], "sent", "{reply}");
        let resize = send_request(
            &mut ws,
            &mut s,
            "shell.resize",
            json!({"shell_id":rig.shell,"columns":43,"rows":17}),
        )
        .await;
        let reply = expect_response(&mut ws, &mut s, &resize).await;
        assert_eq!(reply["ok"], true, "{reply}");
        let list = send_request(&mut ws, &mut s, "projects.list", json!({})).await;
        assert_eq!(expect_response(&mut ws, &mut s, &list).await["ok"], true);
        let clear = send_request(
            &mut ws,
            &mut s,
            "shell.resize.clear",
            json!({"shell_id":rig.shell}),
        )
        .await;
        assert_eq!(expect_response(&mut ws, &mut s, &clear).await["ok"], true);
        assert_eq!(rig.count("shell keys"), 1);
        assert_eq!(
            rig.output_calls(),
            1,
            "the wait is still the only output call"
        );

        // The wait, sent first, is answered last, by its own id.
        touch(&rig.dir, "gate");
        let reply = expect_response(&mut ws, &mut s, &wait).await;
        assert_eq!(reply["result"]["hash"], "0123456789abcdef", "{reply}");
        assert_eq!(reply["result"]["output"], "hi\n");
    }

    fn history_params(shell: &str) -> Value {
        json!({"shell_id":shell,"end":0,"lines":100})
    }

    #[tokio::test]
    async fn history_pages_in_flight_do_not_hold_up_typing_resizing_or_reads() {
        let rig = Rig::new().await;
        let (mut ws, mut s) = mobile(&rig.pairing).await;
        // Three pages, all stuck in their CLI: every shared slot is taken.
        let mut pages = Vec::new();
        for _ in 0..3 {
            pages.push(
                send_request(&mut ws, &mut s, "shell.history", history_params(&rig.shell)).await,
            );
        }
        eventually(|| rig.count("shell history") == 3).await;

        // Typing and resizing have a slot of their own, so they are answered
        // now, as the next frames, none of them queued behind a page.
        let keys = send_request(
            &mut ws,
            &mut s,
            "shell.keys",
            keys_params(&rig.shell, "typed"),
        )
        .await;
        let reply = expect_response(&mut ws, &mut s, &keys).await;
        assert_eq!(reply["result"]["status"], "sent", "{reply}");
        let resize = send_request(
            &mut ws,
            &mut s,
            "shell.resize",
            json!({"shell_id":rig.shell,"columns":43,"rows":17}),
        )
        .await;
        assert_eq!(expect_response(&mut ws, &mut s, &resize).await["ok"], true);
        assert_eq!(rig.count("shell keys"), 1);
        // A fourth page waits for a slot, in arrival order, and is not started.
        let fourth =
            send_request(&mut ws, &mut s, "shell.history", history_params(&rig.shell)).await;
        sleep(Duration::from_millis(400)).await;
        assert_eq!(rig.count("shell history"), 3, "a fourth page started");

        touch(&rig.dir, "history-gate");
        let mut answered = Vec::new();
        for _ in 0..4 {
            let response = next_response(&mut ws, &mut s).await;
            assert_eq!(response["ok"], true, "{response}");
            assert_eq!(
                response["result"],
                json!({"shell_id":rig.shell,"output":"1\n2","line_count":2,
                       "history_size":10,"complete":false}),
                "{response}"
            );
            answered.push(response["id"].as_str().unwrap().to_owned());
        }
        let mut expected = pages.clone();
        expected.push(fourth);
        answered.sort();
        expected.sort();
        assert_eq!(answered, expected, "every page is answered once, by id");
        assert_eq!(rig.count("shell history"), 4);
        let call = std::fs::read_to_string(rig.dir.join("cli.log"))
            .unwrap()
            .lines()
            .find(|line| line.starts_with("shell history"))
            .unwrap()
            .to_owned();
        assert_eq!(
            call,
            format!("shell history {} --end 0 --lines 100 --json", rig.shell)
        );
    }

    #[tokio::test]
    async fn a_history_page_does_not_take_a_wait_slot() {
        let rig = Rig::new().await;
        let (mut ws, mut s) = mobile(&rig.pairing).await;
        // Two waits hold both poll slots; a page still runs in the third.
        let mut waits = Vec::new();
        for _ in 0..2 {
            waits.push(
                send_request(
                    &mut ws,
                    &mut s,
                    "shell.output",
                    wait_params(&rig.shell, 10_000),
                )
                .await,
            );
        }
        eventually(|| rig.output_calls() == 2).await;
        let page = send_request(&mut ws, &mut s, "shell.history", history_params(&rig.shell)).await;
        eventually(|| rig.count("shell history") == 1).await;
        touch(&rig.dir, "history-gate");
        let reply = expect_response(&mut ws, &mut s, &page).await;
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["result"]["line_count"], 2);
        touch(&rig.dir, "gate");
        for _ in 0..2 {
            let reply = next_response(&mut ws, &mut s).await;
            assert!(waits.contains(&reply["id"].as_str().unwrap().to_owned()));
        }
    }

    #[tokio::test]
    async fn an_unchanged_wait_is_answered_without_output() {
        let rig = Rig::new().await;
        touch(&rig.dir, "gate");
        touch(&rig.dir, "unchanged");
        let (mut ws, mut s) = mobile(&rig.pairing).await;
        let reply = call(
            &mut ws,
            &mut s,
            "shell.output",
            wait_params(&rig.shell, 2_000),
        )
        .await;
        assert_eq!(
            reply["result"],
            json!({"shell_id":rig.shell,"unchanged":true,"hash":"0123456789abcdef"}),
            "{reply}"
        );
        let logged = std::fs::read_to_string(rig.dir.join("cli.log")).unwrap();
        let call_line = logged
            .lines()
            .find(|line| line.starts_with("shell output"))
            .unwrap();
        assert!(
            call_line.contains("--if-changed=0123456789abcdef --wait-ms 2000 --json"),
            "{call_line}"
        );
    }

    #[tokio::test]
    async fn at_most_four_requests_run_at_once_and_the_rest_wait_their_turn() {
        let rig = Rig::new().await;
        let (mut ws, mut s) = mobile(&rig.pairing).await;
        // Five plain reads, all stuck in their CLI: three run, two queue.
        let mut reads = Vec::new();
        for _ in 0..5 {
            reads.push(
                send_request(
                    &mut ws,
                    &mut s,
                    "shell.output",
                    rig.output_request(json!({})),
                )
                .await,
            );
        }
        eventually(|| rig.output_calls() == 3).await;
        sleep(Duration::from_millis(400)).await;
        assert_eq!(rig.output_calls(), 3, "a fourth read started");

        // Typing has a slot of its own, however many reads are stuck.
        let keys = send_request(
            &mut ws,
            &mut s,
            "shell.keys",
            keys_params(&rig.shell, "typed"),
        )
        .await;
        expect_response(&mut ws, &mut s, &keys).await;
        // A sixth read queues behind the others, not in front of them.
        let late = send_request(&mut ws, &mut s, "projects.list", json!({})).await;
        sleep(Duration::from_millis(300)).await;
        assert_eq!(rig.output_calls(), 3);

        touch(&rig.dir, "gate");
        let mut answered = Vec::new();
        for _ in 0..6 {
            let response = next_response(&mut ws, &mut s).await;
            assert_eq!(response["ok"], true, "{response}");
            answered.push(response["id"].as_str().unwrap().to_owned());
        }
        let mut expected: Vec<String> = reads.clone();
        expected.push(late);
        answered.sort();
        expected.sort();
        assert_eq!(answered, expected, "every request is answered once, by id");
        assert_eq!(rig.output_calls(), 5);
    }

    #[tokio::test]
    async fn at_most_two_waits_run_and_a_third_read_slot_stays_free() {
        let rig = Rig::new().await;
        let (mut ws, mut s) = mobile(&rig.pairing).await;
        let mut waits = Vec::new();
        for _ in 0..3 {
            waits.push(
                send_request(
                    &mut ws,
                    &mut s,
                    "shell.output",
                    wait_params(&rig.shell, 10_000),
                )
                .await,
            );
        }
        eventually(|| rig.output_calls() == 2).await;
        sleep(Duration::from_millis(400)).await;
        assert_eq!(rig.output_calls(), 2, "a third wait started");
        // Reads still get through while the waits hold their slots.
        let list = send_request(
            &mut ws,
            &mut s,
            "shells.list",
            json!({"project_id":uuid::Uuid::new_v4().to_string()}),
        )
        .await;
        expect_response(&mut ws, &mut s, &list).await;
        let read = send_request(
            &mut ws,
            &mut s,
            "shell.output",
            rig.output_request(json!({"if_changed":"x"})),
        )
        .await;
        // A plain read (no wait) needs the CLI, which is gated too: it runs
        // in the third slot, which is why the wait limit is below the slots.
        eventually(|| rig.output_calls() == 3).await;
        touch(&rig.dir, "gate");
        let mut answered = Vec::new();
        for _ in 0..4 {
            answered.push(
                next_response(&mut ws, &mut s).await["id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        let mut expected = waits.clone();
        expected.push(read);
        answered.sort();
        expected.sort();
        assert_eq!(answered, expected);
        assert_eq!(rig.output_calls(), 4);
    }

    #[tokio::test]
    async fn typing_and_resizing_run_one_at_a_time_in_the_order_they_were_sent() {
        let rig = Rig::new().await;
        touch(&rig.dir, "keys-slow");
        let (mut ws, mut s) = mobile(&rig.pairing).await;
        let first = send_request(
            &mut ws,
            &mut s,
            "shell.keys",
            keys_params(&rig.shell, "one"),
        )
        .await;
        let resize = send_request(
            &mut ws,
            &mut s,
            "shell.resize",
            json!({"shell_id":rig.shell,"columns":50,"rows":20}),
        )
        .await;
        let second = send_request(
            &mut ws,
            &mut s,
            "shell.keys",
            keys_params(&rig.shell, "two"),
        )
        .await;
        // Answered in the order sent, since each had to wait for the one before.
        for id in [&first, &resize, &second] {
            assert_eq!(expect_response(&mut ws, &mut s, id).await["ok"], true);
        }
        let order = std::fs::read_to_string(rig.dir.join("order.log")).unwrap();
        assert_eq!(
            order.lines().collect::<Vec<_>>(),
            [
                "begin keys t:one",
                "end keys t:one",
                "begin resize",
                "end resize",
                "begin keys t:two",
                "end keys t:two",
            ],
            "{order}"
        );
    }

    #[tokio::test]
    async fn revoking_the_device_ends_a_pending_wait_and_its_cli_process() {
        let mut rig = Rig::new().await;
        let (mut ws, mut s) = mobile(&rig.pairing).await;
        send_request(
            &mut ws,
            &mut s,
            "shell.output",
            wait_params(&rig.shell, 10_000),
        )
        .await;
        eventually(|| rig.output_calls() == 1).await;
        let pids = rig.output_pids();
        assert_eq!(pids.len(), 1);
        assert!(alive(&pids[0]));

        let revoked = Instant::now();
        rig.storage.revoke(&rig.pairing.device_id).unwrap();
        let outcome = timeout(Duration::from_secs(5), &mut rig.connector)
            .await
            .expect("the connection closes long before the wait would have ended")
            .unwrap();
        let reason = format!("{:#}", outcome.unwrap_err());
        assert!(reason.contains("revoked"), "{reason}");
        assert!(revoked.elapsed() < Duration::from_secs(5));
        // The wait's CLI is not left running until its own timeout.
        eventually(|| !alive(&pids[0])).await;
    }

    #[tokio::test]
    async fn the_phone_disconnecting_ends_a_pending_wait_and_the_next_session_starts_clean() {
        let rig = Rig::new().await;
        let (mut ws, mut s) = mobile(&rig.pairing).await;
        send_request(
            &mut ws,
            &mut s,
            "shell.output",
            wait_params(&rig.shell, 10_000),
        )
        .await;
        // Typing that is still running when the phone goes away.
        touch(&rig.dir, "keys-block");
        send_request(
            &mut ws,
            &mut s,
            "shell.keys",
            keys_params(&rig.shell, "late"),
        )
        .await;
        eventually(|| rig.output_calls() == 1 && rig.count("shell keys") == 1).await;
        let pids = rig.output_pids();
        ws.close(None).await.unwrap();
        drop(ws);
        // The wait goes with the session; the connector itself keeps running.
        eventually(|| !alive(&pids[0])).await;
        assert!(!rig.connector.is_finished());

        // A new session gets its own answers and never the old session's.
        let (mut ws, mut s) = mobile(&rig.pairing).await;
        let list = send_request(&mut ws, &mut s, "projects.list", json!({})).await;
        assert_eq!(expect_response(&mut ws, &mut s, &list).await["ok"], true);
        // The old typing finishes now; its answer has nowhere to go.
        touch(&rig.dir, "keys-gate");
        eventually(|| logged(&rig.dir, "order.log", "end keys") == 1).await;
        let again = send_request(&mut ws, &mut s, "projects.list", json!({})).await;
        assert_eq!(expect_response(&mut ws, &mut s, &again).await["ok"], true);
    }
}
