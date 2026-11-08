use crate::{
    HANDSHAKE_SECONDS, MAX_FRAME, MAX_PLAINTEXT,
    config::{Device, DeviceKind, Storage},
    crypto::{
        ClientFinish, ClientHello, ClientHelloV2, Envelope, PairFinish, PairHello, Pending,
        Session, accept_client_hello_v2, accept_hello, decode, random32,
    },
    lanes::{Lane, Lanes, MAX_QUEUED, Target, classify, target_of},
    link::{self, EncodeError},
    log_safe,
    pty::{self, PtySet},
    rpc::{Reply, Rpc},
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
        "--somtus must name an existing absolute Somtus executable path"
    );
    let _exclusive = storage.lock("connector.lock")?;
    let rpc = Arc::new(Rpc::new(cli, storage.clone()));
    let mut running: HashMap<String, (watch::Sender<bool>, tokio::task::JoinHandle<()>)> =
        HashMap::new();
    let mut tick = interval(Duration::from_millis(250));
    // The uploads' sweep: at once, then every hour (see `upload`).
    let mut sweep = interval(Duration::from_secs(60 * 60));
    eprintln!(
        "Somtus connector running; paired device config is watched; Ctrl-C stops transport only."
    );
    loop {
        tokio::select! {
            _=tokio::signal::ctrl_c()=>break,
            _=sweep.tick()=>{
                let rpc=rpc.clone();
                tokio::spawn(async move{rpc.sweep_uploads().await;});
            }
            _=tick.tick()=>{
                let cfg=match storage.config(){Ok(c)=>c,Err(e)=>{
                    // Fail closed if protected config disappears/corrupts permissions.
                    for (_, (cancel,_)) in running.drain(){let _=cancel.send(true);}
                    return Err(e);
                }};
                let active:HashMap<_,_>=cfg.devices.into_iter().filter(|d|!d.revoked).map(|d|(d.pairing.device_id.clone(),d)).collect();
                let removed=running.keys().filter(|id|!active.contains_key(*id)).cloned().collect::<Vec<_>>();
                for id in removed {if let Some((cancel,_))=running.remove(&id){let _=cancel.send(true);eprintln!("Remote device {id} was removed or revoked; connection closed.");rpc.forget_uploads(&id).await;rpc.forget_device(&id);}}
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

/// The terminal streams of the session in progress, if the device may have any
/// (a desktop on protocol 2). Whatever way the session ends, its streams end
/// with it and their client processes are killed: a peer change, a revoked
/// device, any error that closes the connection, or the connector stopping.
#[derive(Default)]
struct SessionStreams(Option<Arc<PtySet>>);
impl SessionStreams {
    /// A new session begins. `allowed`: whether this device may open streams.
    fn begin(&mut self, allowed: bool) {
        self.end();
        self.0 = allowed.then(PtySet::new);
    }
    fn end(&mut self) {
        if let Some(set) = self.0.take() {
            set.close_all();
        }
    }
}
impl Drop for SessionStreams {
    fn drop(&mut self) {
        self.end();
    }
}

/// A received request waiting for a slot; see `lanes`.
struct Queued {
    /// The session it arrived in.
    epoch: u64,
    /// Whose slots it takes.
    target: Target,
    /// When the phone has to have an answer, for a request that goes to one of its
    /// hosts (see `gateway::deadline`).
    deadline: Option<std::time::Instant>,
    /// When it stops waiting for its turn (see `gateway::queue_deadline`).
    queue_deadline: Option<std::time::Instant>,
    viewport: SharedViewport,
    request: Value,
    /// When it was decrypted: the start of the reply's `server_ms`.
    received: std::time::Instant,
    /// The most JSON its response may hold (see `link`).
    reply_limit: usize,
    /// The terminal streams of the session, for a device that may have any.
    pty: Option<Arc<PtySet>>,
    gateway: Arc<crate::gateway::Gateway>,
}

/// How a request ended. `outcome` is `None` when it was cut short because its
/// session ended; an `Err` closes the connection, as it always did.
struct Done {
    lane: Lane,
    target: Target,
    epoch: u64,
    received: std::time::Instant,
    outcome: Option<Result<Reply>>,
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
                target,
                deadline,
                queue_deadline: _,
                viewport,
                request,
                received,
                reply_limit,
                pty,
                gateway,
            } = queued;
            let run = async {
                if request["method"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("hosts."))
                {
                    rpc.handle_gateway_reply(&device, request, &gateway, deadline, reply_limit)
                        .await
                } else {
                    rpc.handle_session_reply(&device, request, &viewport, pty.as_ref(), reply_limit)
                        .await
                }
            };
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
                target,
                epoch,
                received,
                outcome,
            }
        });
    }
}

/// Answers what is still queued at its deadline (see `gateway::queue_deadline`), with what
/// its target ordered after it (see `Lanes::take_queued`). Run before a slot is handed on,
/// and not only on the timer: a request for a host that late could only be refused once
/// started, and its slot would then go to what came after it.
async fn answer_late(
    lanes: &mut Lanes<Queued>,
    renewing: &mut JoinSet<Option<&'static str>>,
    epoch: u64,
    session: &mut Option<Session>,
    ws: &mut Socket,
    compress: bool,
) -> Result<()> {
    let now = std::time::Instant::now();
    for late in lanes.take_queued(|queued| queued.queue_deadline.is_some_and(|at| at <= now)) {
        if late.epoch != epoch {
            continue;
        }
        // The phone would give up on these, and on the whole connection with them. They
        // never started, so what it is told is that nothing was sent, but for a release.
        let answer = end_pin_of_late_release(&late, renewing)
            .unwrap_or_else(|| crate::gateway::not_sent(&late.request));
        let s = session.as_mut().context("response without a session")?;
        let sealed = seal_reply(s, answer, late.received, compress).await?;
        send_json(ws, &sealed).await?;
    }
    Ok(())
}
/// A release that will not run ends the pin all the same, as one that failed does (see
/// `shell.resize.clear`), and is answered as one that ran. Its client may have forgotten the
/// shell by now, and its traffic would go on renewing; one that only covered the terminal
/// would otherwise go on believing it holds the size, and not ask for it when it comes
/// back. This waits for a renewal that is running, as the release would have, so it goes
/// with those; what is left of the lease lapses. `None` for any other request.
fn end_pin_of_late_release(
    late: &Queued,
    renewing: &mut JoinSet<Option<&'static str>>,
) -> Option<Value> {
    if late.request["method"] != "shell.resize.clear" {
        return None;
    }
    let uuid = |value: &Value| {
        value
            .as_str()
            .filter(|text| crate::crypto::uuid(text).is_ok())
            .map(str::to_owned)
    };
    let id = uuid(&late.request["id"])?;
    let shell = uuid(&late.request["params"]["shell_id"])?;
    let viewport = late.viewport.clone();
    let held = shell.clone();
    renewing.spawn(async move {
        if let Some(v) = viewport.lock().await.as_mut()
            && v.selected
                .as_ref()
                .is_some_and(|(selected, _, _)| *selected == held)
        {
            v.give_up();
        }
        None
    });
    Some(crate::rpc::success(
        &id,
        json!({"shell_id":shell,"status":"cleared"}),
    ))
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
    let reply = Reply {
        id: response.get("id").cloned().unwrap_or(Value::Null),
        body: serde_json::to_vec(&response)?,
    };
    seal_body(session, reply, received, compress).await
}
/// `seal_reply` for a response a request's task has already serialized.
async fn seal_body(
    session: &mut Session,
    reply: Reply,
    received: std::time::Instant,
    compress: bool,
) -> Result<crate::crypto::Envelope> {
    let Reply { id, body } = reply;
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
            let error = crate::rpc::too_large(id);
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
    // Whether the CLI has chats is learned while the phone is still on its way, so that `ready`
    // does not wait for the first run of a CLI that was just updated. (Not before the relay
    // has been reached: a connector that cannot reach it tries again every second.)
    {
        let rpc = rpc.clone();
        tokio::spawn(async move { rpc.learn_chat().await });
    }
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
    // Only a desktop on protocol 2 may open terminal streams (`pty`). Dropped
    // with this function, however it ends, which kills every client process.
    let streams_allowed = device.kind == DeviceKind::Desktop && p.v == 2;
    let mut streams = SessionStreams::default();
    let mut gateway = crate::gateway::SessionGateway::default();
    // Request tasks; dropped (so aborted) with this function, however it ends.
    let mut tasks: JoinSet<Done> = JoinSet::new();
    // What holds the viewport outside a request: the lease's renewal while it runs (at
    // most one) and the end of a pin. Dropped the same way.
    let mut renewing: JoinSet<Option<&'static str>> = JoinSet::new();
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
                // A resize that is running renews the lease itself, and so does a renewal
                // that has not finished. The CLI runs in a task of its own, holding the
                // viewport as a resize would, so that the loop goes on reading, answering
                // and pinging meanwhile.
                if renewing.is_empty()
                    && let Some(shared)=&viewport
                    && last_mobile.elapsed()<=timing.mobile_active
                    && let Ok(mut held)=shared.clone().try_lock_owned()
                    && held.as_ref().is_some_and(|v| v.selected.is_some())
                {
                    let rpc=rpc.clone();
                    renewing.spawn(async move {
                        match held.as_mut() {
                            Some(v)=>rpc.renew_lease(v).await,
                            None=>None,
                        }
                    });
                }
                continue;
            },
            _=heartbeat.tick()=>{timeout(Duration::from_secs(10),ws.send(Message::Ping(vec![].into()))).await??;continue;},
            _=revocation.tick(), if !tasks.is_empty() || lanes.queued()>0 =>{
                // A long wait must not outlive the device's authorization.
                ensure!(rpc.storage.authorized(&p.device_id)?, "device revoked during RPC");
                answer_late(&mut lanes,&mut renewing,epoch,&mut session,&mut ws,compress).await?;
                continue;
            },
            Some(joined)=tasks.join_next(), if !tasks.is_empty() =>{
                let done=joined.context("request task failed")?;
                lanes.finished_for(&done.target,done.lane);
                answer_late(&mut lanes,&mut renewing,epoch,&mut session,&mut ws,compress).await?;
                start_ready(&mut lanes,&mut tasks,rpc,&p.device_id,&session_ended);
                if done.epoch==epoch && let Some(outcome)=done.outcome {
                    let reply=outcome?;
                    ensure!(
                        rpc.storage.authorized(&p.device_id)?,
                        "device revoked during RPC"
                    );
                    let s=session.as_mut().context("response without a session")?;
                    let sealed=seal_body(s,reply,done.received,compress).await?;
                    send_json(&mut ws,&sealed).await?;
                }
                continue;
            },
            Some(renewed)=renewing.join_next(), if !renewing.is_empty() =>{
                if let Some(why)=renewed.context("lease renewal failed")? {
                    eprintln!("Remote device {} no longer holds a terminal's size: {why}.",p.device_id);
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
                streams.end();
                gateway.end();
                if let Some(shared) = viewport.take() {
                    // Waits for a resize that is running; the tasks run on their own. A
                    // lease that cannot be released lapses by itself, and the next phone
                    // has no reason to wait for a new connection over it.
                    if let Some(mut v) = shared.lock().await.take() {
                        let _ = rpc.clear_viewport(&mut v).await;
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
                let mut features = rpc.phone_features(None).await;
                features["hosts"] = json!(1);
                if streams_allowed {
                    features["pty"] = pty::features();
                }
                let ready = json!({"v":1,"type":"ready","desktop_id":p.desktop_id,"device_id":p.device_id,"features":features});
                let e = s.seal("d2c", &serde_json::to_vec(&ready)?)?;
                send_json(&mut ws, &e).await?;
                session = Some(s);
                compress = false;
                streams.begin(streams_allowed);
                gateway.begin(rpc.storage.clone(), p.device_id.clone());
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
                // Writing, resizing and closing a terminal stream never wait, so
                // they are answered here, in order, like `link.configure`.
                if let Some(set) = &streams.0
                    && matches!(
                        request.get("method").and_then(Value::as_str),
                        Some("pty.write" | "pty.resize" | "pty.close")
                    )
                {
                    let answer = rpc.handle_pty_inline(&p.device_id, request, set)?;
                    let reply = seal_reply(s, answer, received, compress).await?;
                    send_json(&mut ws, &reply).await?;
                    continue;
                }
                // Closing a routed host is session cleanup. It must not wait
                // behind that host's accepted mutation (or its long polls).
                // SSH finishes accepted ordered work after the link closes;
                // queued calls see the invalid token and never reach the host.
                if request.get("method").and_then(Value::as_str) == Some("hosts.close") {
                    let gateway = gateway
                        .current()
                        .context("RPC before authenticated handshake")?;
                    let reply = rpc
                        .handle_gateway_reply(&p.device_id, request, &gateway, None, MAX_PLAINTEXT)
                        .await?;
                    let sealed = seal_body(s, reply, received, compress).await?;
                    send_json(&mut ws, &sealed).await?;
                    continue;
                }
                let target = target_of(&request);
                let lane = classify(&request);
                let deadline = crate::gateway::deadline(&target, &request, received);
                let queue_deadline = crate::gateway::queue_deadline(
                    &target,
                    lane,
                    device.kind == DeviceKind::Desktop,
                    &request,
                    received,
                );
                lanes.push_for(
                    target.clone(),
                    lane,
                    Queued {
                        epoch,
                        target,
                        deadline,
                        queue_deadline,
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
                        pty: streams.0.clone(),
                        gateway: gateway
                            .current()
                            .context("RPC before authenticated handshake")?,
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

    /// Stand-in Somtus CLI: logs each call and reports one live shell.
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
        // The first run of a new executable can take seconds on a busy Mac, and `ready` asks
        // the CLI about chats: run it once here, where nothing is timed, and forget the call.
        let _ = std::process::Command::new(&cli).arg("warm").output();
        std::fs::write(&log, "").unwrap();
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
    #[ignore = "slow: real relay, connector and CLI processes; about 5 s of sleeps around the lease window"]
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
            ..Timing::default()
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

    /// The lease renewal runs a CLI every few seconds while the phone shows a terminal.
    /// A request that arrives meanwhile must not wait for it. `--nocapture` prints how
    /// long such a request took.
    #[tokio::test]
    #[ignore = "slow: real relay and CLI processes; times a call made during a 2 s renewal"]
    async fn a_lease_renewal_does_not_hold_up_other_requests() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
        let (storage, device, routes) = paired(tmp.path(), &url).await;
        let relay = Relay::new(private_read::<Routes>(&routes, 1 << 20).unwrap(), 8).unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, relay.router()).await });
        let shell = uuid::Uuid::new_v4().to_string();
        let (cli, log, slow) = (
            tmp.path().join("fake-riwork"),
            tmp.path().join("cli.log"),
            tmp.path().join("slow"),
        );
        // `shell resize` takes 2 s once `slow` exists; everything else is quick.
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1 $2\" in\n\
                 'shell list') printf '[{{\"id\":\"{shell}\",\"alive\":true}}]';;\n\
                 'orchestrator list'|'project list') echo '[]';;\n\
                 'shell resize') if [ -e '{}' ]; then sleep 2; fi;;\nesac\n",
                log.display(),
                slow.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::process::Command::new(&cli).arg("warm").output();
        let rpc = Arc::new(Rpc::new(cli, storage));
        let timing = Timing {
            ping: Duration::from_millis(100),
            idle: Duration::from_secs(5),
            renew: Duration::from_millis(200),
            mobile_active: Duration::from_secs(5),
            revoke_poll: Duration::from_millis(50),
            ..Timing::default()
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

        // The second renewal to start after `slow` appears is a slow one.
        let before = renewals(&log);
        std::fs::write(&slow, "").unwrap();
        for _ in 0..1000 {
            if renewals(&log) >= before + 2 {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
        assert!(renewals(&log) >= before + 2, "renewals run");
        let started = std::time::Instant::now();
        let listed = call(&mut ws, &mut session, "projects.list", json!({})).await;
        let took = started.elapsed();
        assert_eq!(listed["ok"], true, "{listed}");
        println!("projects.list during a 2 s lease renewal took {took:?}");
        assert!(took < Duration::from_secs(1), "{took:?}");
        connector.abort();
        server.abort();
    }

    /// `cargo test -p riwork-remote --lib reply_tail_cost -- --ignored --nocapture` prints
    /// what a finished response costs until it is sealed: before, its task serialized it
    /// to check the reply limit and the connection loop serialized it again to seal it;
    /// now the task's text is sealed.
    #[tokio::test]
    #[ignore = "prints timings; run on demand"]
    async fn reply_tail_cost() {
        let id = crate::crypto::Identity {
            desktop_id: uuid::Uuid::new_v4().to_string(),
            device_id: uuid::Uuid::new_v4().to_string(),
            route_id: uuid::Uuid::new_v4().to_string(),
        };
        let (secret, c) = (random32(), random32());
        let hello = client_hello(&id, &secret, c).unwrap();
        let (server, pending) = accept_hello(&id, &secret, &hello, random32()).unwrap();
        let (finish, _) = accept_server(&id, &secret, &c, &server).unwrap();
        let mut session = pending.finish(&finish).unwrap();
        let request = uuid::Uuid::new_v4().to_string();
        let styled: String = (0..400)
            .map(|i| {
                format!(
                    "\u{1b}[38;5;{}mline {i} of the build output\u{1b}[0m\n",
                    i % 200
                )
            })
            .collect();
        let chunk = crate::crypto::b64(&[b'x'; pty::MAX_CHUNK]);
        let events: Vec<Value> = (0..400)
            .map(|seq| json!({"seq":seq,"event":{"event":"agent_message_delta","text":styled[..2000].to_owned()}}))
            .collect();
        for (label, result, compress) in [
            (
                "styled shell.output",
                json!({"shell_id":"s","output":styled,"hash":"0123456789abcdef"}),
                false,
            ),
            (
                "pty.read chunk",
                json!({"stream":"s","seq":0,"data":chunk}),
                false,
            ),
            (
                "chat.events page, deflated",
                json!({"chat_id":"c","events":events,"next":400,"more":false}),
                true,
            ),
        ] {
            let response = json!({"v":1,"type":"response","id":request,"ok":true,"result":result});
            let limit = if compress {
                link::MAX_INFLATED
            } else {
                MAX_PLAINTEXT
            };
            let rounds = 20u32;
            let started = std::time::Instant::now();
            for _ in 0..rounds {
                let response = response.clone();
                assert!(serde_json::to_vec(&response).unwrap().len() <= limit);
                seal_reply(&mut session, response, std::time::Instant::now(), compress)
                    .await
                    .unwrap();
            }
            let before = started.elapsed() / rounds;
            let started = std::time::Instant::now();
            for _ in 0..rounds {
                let response = response.clone();
                let reply = Reply {
                    id: response["id"].clone(),
                    body: serde_json::to_vec(&response).unwrap(),
                };
                assert!(reply.body.len() <= limit);
                seal_body(&mut session, reply, std::time::Instant::now(), compress)
                    .await
                    .unwrap();
            }
            let after = started.elapsed() / rounds;
            println!(
                "{label} ({} bytes of JSON): serialized twice {before:?}, once {after:?}",
                serde_json::to_vec(&response).unwrap().len()
            );
        }
    }

    /// A connector whose phone has pinned a live shell, with a stand-in CLI that fails
    /// the way the real one does: `shell resize` and `shell resize-clear` of that shell
    /// print what the files `resize-says` and `clear-says` hold and exit 2, and `shell
    /// list` no longer lists it once `gone` exists. A second shell lists and pins without
    /// fail. A write takes longer than a phone gives what waits behind it once
    /// `slow-writes` exists. Renewals run every 100 ms.
    struct LeaseRig {
        _tmp: tempfile::TempDir,
        dir: PathBuf,
        log: PathBuf,
        shell: String,
        other: String,
        pairing: Pairing,
        connector: tokio::task::JoinHandle<Result<()>>,
        server: tokio::task::JoinHandle<std::io::Result<()>>,
        ws: Socket,
        session: Session,
    }
    impl LeaseRig {
        async fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().to_path_buf();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
            let (storage, device, routes) = paired(&dir, &url).await;
            let relay = Relay::new(private_read::<Routes>(&routes, 1 << 20).unwrap(), 8).unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, relay.router()).await });
            let shell = uuid::Uuid::new_v4().to_string();
            let other = uuid::Uuid::new_v4().to_string();
            let (cli, log) = (dir.join("lease-cli"), dir.join("cli.log"));
            let script = r#"#!/bin/sh
D='@DIR@'
printf '%s\n' "$*" >> "$D/cli.log"
case "$1 $2" in
capabilities*) echo '{"v":1,"native":{"v":1,"methods":["Store.renameProject"]}}';;
'native invoke') cat >/dev/null; if [ -e "$D/slow-writes" ]; then sleep 16; fi; echo '{"v":1,"value":{}}';;
'shell list') if [ -e "$D/gone" ]; then printf '[{"id":"@OTHER@","alive":true}]'; else printf '[{"id":"@SHELL@","alive":true},{"id":"@OTHER@","alive":true}]'; fi;;
'orchestrator list'|'project list') echo '[]';;
'shell resize') if [ "$3" = '@SHELL@' ] && [ -e "$D/resize-says" ]; then cat "$D/resize-says" >&2; exit 2; fi;;
'shell resize-clear') if [ "$3" = '@SHELL@' ] && [ -e "$D/clear-says" ]; then cat "$D/clear-says" >&2; exit 2; fi;;
esac
"#
            .replace("@DIR@", &dir.to_string_lossy())
            .replace("@SHELL@", &shell)
            .replace("@OTHER@", &other);
            std::fs::write(&cli, script).unwrap();
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
            let _ = std::process::Command::new(&cli).arg("warm").output();
            let rpc = Arc::new(Rpc::new(cli, storage));
            let timing = Timing {
                ping: Duration::from_millis(100),
                idle: Duration::from_secs(5),
                renew: Duration::from_millis(100),
                mobile_active: Duration::from_secs(600),
                revoke_poll: Duration::from_millis(50),
            };
            let pairing = device.pairing.clone();
            let connector =
                tokio::spawn(async move { run_device_with(&device, &rpc, timing).await });
            let (mut ws, mut session) = mobile(&pairing).await;
            let resized = call(
                &mut ws,
                &mut session,
                "shell.resize",
                json!({"shell_id":shell,"columns":43,"rows":17}),
            )
            .await;
            assert_eq!(resized["ok"], true, "{resized}");
            let rig = Self {
                _tmp: tmp,
                dir,
                log,
                shell,
                other,
                pairing,
                connector,
                server,
                ws,
                session,
            };
            rig.renews().await;
            rig
        }
        fn says(&self, file: &str, words: &str) {
            std::fs::write(self.dir.join(file), format!("{words}\n")).unwrap();
        }
        fn heals(&self, file: &str) {
            std::fs::remove_file(self.dir.join(file)).unwrap();
        }
        async fn renews(&self) {
            let before = renewals(&self.log);
            eventually(|| renewals(&self.log) >= before + 2).await;
        }
        /// Waits until no renewal has started for a second, which is many ticks.
        async fn renewals_have_stopped(&self) {
            let mut seen = renewals(&self.log);
            for _ in 0..20 {
                sleep(Duration::from_secs(1)).await;
                let now = renewals(&self.log);
                if now == seen {
                    return;
                }
                seen = now;
            }
            panic!("renewals never stopped");
        }
        /// The connection is the same one and answers: this session's next counter
        /// would not open on any other.
        async fn still_answers(&mut self) {
            assert!(!self.connector.is_finished(), "{:?}", self.connector);
            let listed = call(&mut self.ws, &mut self.session, "projects.list", json!({})).await;
            assert_eq!(listed["ok"], true, "{listed}");
        }
        async fn release(&mut self) -> Value {
            let shell = self.shell.clone();
            call(
                &mut self.ws,
                &mut self.session,
                "shell.resize.clear",
                json!({"shell_id":shell}),
            )
            .await
        }
        async fn pin(&mut self, shell: &str) -> Value {
            call(
                &mut self.ws,
                &mut self.session,
                "shell.resize",
                json!({"shell_id":shell,"columns":43,"rows":17}),
            )
            .await
        }
        /// Asks without waiting for the answer.
        async fn send(&mut self, method: &str, params: Value) -> String {
            let id = uuid::Uuid::new_v4().to_string();
            let request = json!({"v":1,"type":"request","id":id,"method":method,"params":params});
            let sealed = self
                .session
                .seal("c2d", &serde_json::to_vec(&request).unwrap())
                .unwrap();
            send_json(&mut self.ws, &sealed).await.unwrap();
            id
        }
        async fn next(&mut self) -> Value {
            let reply = timeout(Duration::from_secs(25), receive_json(&mut self.ws))
                .await
                .expect("connector answers")
                .unwrap();
            let reply: Envelope = serde_json::from_value(reply).unwrap();
            serde_json::from_slice(&self.session.open("d2c", &reply).unwrap()).unwrap()
        }
    }
    impl Drop for LeaseRig {
        fn drop(&mut self) {
            self.connector.abort();
            self.server.abort();
        }
    }

    #[tokio::test]
    async fn a_shell_that_ended_ends_its_lease_and_not_the_connection() {
        let mut rig = LeaseRig::new().await;
        std::fs::write(rig.dir.join("gone"), "").unwrap();
        rig.says(
            "resize-says",
            &format!("somtus: shell {} has exited", rig.shell),
        );
        rig.renewals_have_stopped().await;
        rig.still_answers().await;
        // The phone has yet to hear of it, and then it lets go of what it thinks it holds:
        // a refusal here would stay in its way for as long as the connection lasts.
        let released = rig.release().await;
        assert_eq!(released["result"]["status"], "cleared", "{released}");
        // Asked again, it is any other shell that is not there.
        let again = rig.release().await;
        assert_eq!(again["error"]["code"], "not_found", "{again}");
    }

    #[tokio::test]
    async fn a_lease_someone_else_took_comes_back_when_they_leave_and_is_not_fought_over() {
        let mut rig = LeaseRig::new().await;
        let busy =
            "somtus: viewport_busy: another authenticated connection owns this terminal size";
        rig.says("resize-says", busy);
        // Asked on every tick. The CLI refuses for as long as the other lease lasts, so this
        // changes nobody's terminal, and the phone goes on believing it holds the size.
        rig.renews().await;
        rig.still_answers().await;
        // The other holder left: the next renewal takes the lease, with no word from the phone.
        rig.heals("resize-says");
        rig.renews().await;
        rig.says("resize-says", busy);
        rig.renews().await;
        // There is nothing of this connection's to let go of, which is what the phone wants.
        rig.says("clear-says", busy);
        let released = rig.release().await;
        assert_eq!(released["result"]["status"], "cleared", "{released}");
        rig.renewals_have_stopped().await;
        let tries = renewals(&rig.log);
        rig.still_answers().await;
        assert_eq!(renewals(&rig.log), tries);
        // Asking for the size while it is held is refused, and that is not held on to.
        let shell = rig.shell.clone();
        let refused = rig.pin(&shell).await;
        assert_eq!(refused["error"]["code"], "viewport_busy", "{refused}");
        rig.renewals_have_stopped().await;
        // Asking for it again asks for the lease again.
        rig.heals("resize-says");
        let resized = rig.pin(&shell).await;
        assert_eq!(resized["ok"], true, "{resized}");
        rig.renews().await;
    }

    #[tokio::test]
    async fn a_lease_someone_else_took_does_not_stop_a_resize_of_another_shell() {
        let mut rig = LeaseRig::new().await;
        let busy =
            "somtus: viewport_busy: another authenticated connection owns this terminal size";
        rig.says("resize-says", busy);
        rig.says("clear-says", busy);
        rig.renews().await;
        let (shell, other) = (rig.shell.clone(), rig.other.clone());
        let resized = rig.pin(&other).await;
        assert_eq!(resized["ok"], true, "{resized}");
        // Only the new one is renewed now.
        let of = |shell: &str| logged(&rig.dir, "cli.log", &format!("shell resize {shell}"));
        let (first, second) = (of(&shell), of(&other));
        eventually(|| of(&other) >= second + 2).await;
        assert_eq!(of(&shell), first);
        // And the first can still be let go of, as a phone that never heard of any of it does.
        let released = rig.release().await;
        assert_eq!(released["result"]["status"], "cleared", "{released}");
    }

    #[tokio::test]
    async fn a_terminal_that_cannot_be_held_is_let_go_at_once() {
        let mut rig = LeaseRig::new().await;
        rig.says(
            "resize-says",
            "somtus: viewport_unsupported: expected one existing pane in window 0",
        );
        // A renewal that failed this way has already extended the lease. Its CLI shows only
        // once it has started, so the quiet that follows proves nothing about it.
        eventually(|| logged(&rig.dir, "cli.log", "shell resize-clear") >= 1).await;
        rig.renewals_have_stopped().await;
        rig.still_answers().await;
        let released = rig.release().await;
        assert_eq!(released["result"]["status"], "cleared", "{released}");
    }

    #[tokio::test]
    async fn a_renewal_that_fails_for_no_stated_reason_goes_on_until_it_works() {
        let mut rig = LeaseRig::new().await;
        rig.says("resize-says", "somtus: tmux did not answer");
        // The phone believes it holds the size and will not ask again, so this has no end
        // but its going quiet, and a later success takes a fresh lease. So it outlasts the
        // CLI's lease (12 s), and renewals still go on.
        sleep(Duration::from_secs(13)).await;
        rig.renews().await;
        rig.still_answers().await;
        rig.heals("resize-says");
        rig.renews().await;
        // It was never given up, so letting go of it asks the CLI to.
        let released = rig.release().await;
        assert_eq!(released["result"]["status"], "cleared", "{released}");
        assert_eq!(logged(&rig.dir, "cli.log", "shell resize-clear"), 1);
        rig.renewals_have_stopped().await;
    }

    #[tokio::test]
    async fn a_release_that_fails_ends_the_pin_and_is_answered_as_one_that_did() {
        let mut rig = LeaseRig::new().await;
        rig.says("clear-says", "somtus: tmux did not answer");
        // Its client may have forgotten the shell by now (a phone leaving for one of this
        // gateway's routes does), and its traffic would keep the lease going for good. One
        // that only covered the terminal goes on believing it holds the size until it is
        // told otherwise, and does not ask for it again when it comes back.
        let released = rig.release().await;
        let shell = rig.shell.clone();
        assert_eq!(
            released["result"],
            json!({"shell_id":shell,"status":"cleared"}),
            "{released}"
        );
        rig.renewals_have_stopped().await;
        let tries = renewals(&rig.log);
        rig.still_answers().await;
        let retried = rig.release().await;
        assert_eq!(retried["result"]["status"], "cleared", "{retried}");
        assert_eq!(renewals(&rig.log), tries);
        // A shell this connection holds nothing of is not its to answer for.
        let again = rig.release().await;
        assert_eq!(again["error"]["code"], "cli_error", "{again}");
    }

    #[tokio::test]
    async fn a_resize_that_failed_and_could_not_be_undone_holds_nothing() {
        let mut rig = LeaseRig::new().await;
        let released = rig.release().await;
        assert_eq!(released["result"]["status"], "cleared", "{released}");
        rig.says("resize-says", "somtus: tmux did not answer");
        rig.says("clear-says", "somtus: tmux did not answer");
        let shell = rig.shell.clone();
        let failed = rig.pin(&shell).await;
        assert_eq!(failed["error"]["code"], "cli_error", "{failed}");
        // Its client was told this failed and will not let go of it: a renewal that goes
        // through later on would leave the terminal at that size with nobody to release it.
        rig.heals("resize-says");
        rig.renewals_have_stopped().await;
        let tries = renewals(&rig.log);
        rig.still_answers().await;
        assert_eq!(renewals(&rig.log), tries);
        let resized = rig.pin(&shell).await;
        assert_eq!(resized["ok"], true, "{resized}");
        rig.renews().await;
    }

    #[tokio::test]
    async fn a_release_that_waited_too_long_to_run_still_ends_the_pin() {
        let mut rig = LeaseRig::new().await;
        std::fs::write(rig.dir.join("slow-writes"), "").unwrap();
        // This machine's one ordered slot is taken for longer than a phone gives what
        // waits behind it.
        let rename = json!({"module":"Store","method":"renameProject","args":{}});
        let write = rig.send("native.invoke", rename).await;
        eventually(|| logged(&rig.dir, "cli.log", "native invoke") >= 1).await;
        let shell = rig.shell.clone();
        let release = rig
            .send("shell.resize.clear", json!({"shell_id":shell}))
            .await;
        // Answered before the phone's timer, and as a release that ran: a phone that only
        // covered the terminal would otherwise go on believing it holds the size, and not
        // ask for it again when it comes back.
        let reply = rig.next().await;
        assert_eq!(reply["id"], release, "{reply}");
        assert_eq!(
            reply["result"],
            json!({"shell_id":shell,"status":"cleared"}),
            "{reply}"
        );
        assert_eq!(logged(&rig.dir, "cli.log", "shell resize-clear"), 0);
        rig.renewals_have_stopped().await;
        let answered = rig.next().await;
        assert_eq!(answered["id"], write, "{answered}");
        assert_eq!(answered["ok"], true, "{answered}");
        // Asked again, it is told the same.
        let released = rig.release().await;
        assert_eq!(released["result"]["status"], "cleared", "{released}");
    }

    /// Typing that waited too long is not sent, and what the phone sent after it must not
    /// reach the shell ahead of it: the phone sends it again later, after a paste perhaps.
    #[tokio::test]
    async fn what_waits_behind_a_request_that_was_not_sent_is_not_sent_either() {
        let mut rig = LeaseRig::new().await;
        std::fs::write(rig.dir.join("slow-writes"), "").unwrap();
        let rename = json!({"module":"Store","method":"renameProject","args":{}});
        let write = rig.send("native.invoke", rename).await;
        eventually(|| logged(&rig.dir, "cli.log", "native invoke") >= 1).await;
        let (shell, other) = (rig.shell.clone(), rig.other.clone());
        // On the phone's 15 s timer, and then one on its 30 s timer, not yet late when the
        // first is.
        let typed = rig
            .send("shell.input", json!({"shell_id":shell,"line":"cat "}))
            .await;
        let later = rig.send("shell.close", json!({"shell_id":other})).await;
        for id in [&typed, &later] {
            let reply = rig.next().await;
            assert_eq!(reply["id"], *id, "{reply}");
            assert_eq!(reply["error"]["code"], "host_timeout", "{reply}");
            let words = reply["error"]["message"].as_str().unwrap();
            assert!(words.contains("not sent"), "{reply}");
        }
        let answered = rig.next().await;
        assert_eq!(answered["id"], write, "{answered}");
        // Nothing of either runs once the slot is free: the next answer is this one's.
        rig.still_answers().await;
    }

    #[tokio::test]
    async fn a_lease_that_cannot_be_released_when_the_phone_leaves_ends_nothing() {
        let mut rig = LeaseRig::new().await;
        rig.says("clear-says", "somtus: tmux did not answer");
        rig.ws.close(None).await.unwrap();
        // The connector goes on and takes the next session, on the same socket to the relay.
        eventually(|| logged(&rig.dir, "cli.log", "shell resize-clear") >= 1).await;
        sleep(Duration::from_millis(200)).await;
        assert!(!rig.connector.is_finished(), "{:?}", rig.connector);
        let (mut ws, mut session) = mobile(&rig.pairing).await;
        let listed = call(&mut ws, &mut session, "projects.list", json!({})).await;
        assert_eq!(listed["ok"], true, "{listed}");
        assert!(!rig.connector.is_finished(), "{:?}", rig.connector);
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
            .push(rcgen::DnType::CommonName, "Somtus Test CA");
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
                ..Timing::default()
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
    #[ignore = "slow: real relay and CLI processes; waits for a killed process to be reaped, fails under load"]
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
    #[ignore = "slow: real relay and CLI processes; waits for a killed process to be reaped, fails under load"]
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
