use crate::{
    HANDSHAKE_SECONDS, MAX_FRAME,
    config::{Device, Storage},
    crypto::{
        ClientFinish, ClientHello, ClientHelloV2, Envelope, PairFinish, PairHello, Pending,
        Session, accept_client_hello_v2, accept_hello, decode, random32,
    },
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
        connect_async_tls_with_config(relay, Some(config), false, wss_connector(relay)),
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
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            ping: Duration::from_secs(20),
            idle: Duration::from_secs(60),
            renew: Duration::from_secs(3),
            mobile_active: Duration::from_secs(20),
        }
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
async fn run_device(device: &Device, rpc: &Rpc) -> Result<()> {
    run_device_with(device, rpc, Timing::default()).await
}
pub(crate) async fn run_device_with(device: &Device, rpc: &Rpc, timing: Timing) -> Result<()> {
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
    let mut viewport: Option<Viewport> = None;
    let mut deadline: Option<Instant> =
        online.then(|| Instant::now() + Duration::from_secs(HANDSHAKE_SECONDS));
    let mut heartbeat = interval(timing.ping);
    let mut renew = interval(timing.renew);
    heartbeat.tick().await;
    let mut last_rx = Instant::now();
    let mut last_mobile = Instant::now();
    loop {
        let idle_at = last_rx + timing.idle;
        // Biased: frames already waiting after a slow RPC must be read before the
        // idle deadline is judged. Ticks come first so a busy peer cannot starve them.
        let value = tokio::select! {
            biased;
            _=renew.tick()=>{
                if let Some(v)=&viewport && last_mobile.elapsed()<=timing.mobile_active {rpc.renew_viewport(v).await?;}
                continue;
            },
            _=heartbeat.tick()=>{timeout(Duration::from_secs(10),ws.send(Message::Ping(vec![].into()))).await??;continue;},
            value=receive_json_seen(&mut ws,&mut last_rx)=>value?,
            _=tokio::time::sleep_until(idle_at)=>{
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
                // Ordered peer control messages reset old transport state.
                if let Some(mut v) = viewport.take() {
                    rpc.clear_viewport(&mut v).await?;
                }
                session = None;
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
                let ready =
                    json!({"v":1,"type":"ready","desktop_id":p.desktop_id,"device_id":p.device_id});
                let e = s.seal("d2c", &serde_json::to_vec(&ready)?)?;
                send_json(&mut ws, &e).await?;
                session = Some(s);
                viewport = Some(Viewport::new(rpc.cli.clone(), p.device_id.clone()));
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
                last_mobile = Instant::now();
                // Unparseable plaintext is answered as an invalid request, not a dropped session.
                let request = serde_json::from_slice(&plaintext).unwrap_or(Value::Null);
                let response = rpc
                    .handle_in(&p.device_id, request, viewport.as_mut())
                    .await?;
                ensure!(
                    rpc.storage.authorized(&p.device_id)?,
                    "device revoked during RPC"
                );
                let reply = s.seal("d2c", &serde_json::to_vec(&response)?)?;
                send_json(&mut ws, &reply).await?;
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
        let rpc = Rpc::new(cli, storage.clone());
        // Short enough to test. Pings every 100 ms against a 1 s idle deadline also
        // prove a healthy but quiet connection is not mistaken for a dead one.
        let timing = Timing {
            ping: Duration::from_millis(100),
            idle: Duration::from_secs(1),
            renew: Duration::from_millis(50),
            mobile_active: Duration::from_secs(1),
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
        let rpc = Rpc::new(cli, storage);
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
        let rpc = Rpc::new(cli, storage.clone());
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
        let rpc = Rpc::new(
            scripted_cli(tmp.path(), &uuid::Uuid::new_v4().to_string()).0,
            storage.clone(),
        );
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
        let rpc = Rpc::new(
            scripted_cli(tmp.path(), &uuid::Uuid::new_v4().to_string()).0,
            storage.clone(),
        );
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
        let rpc = Rpc::new(cli, storage.clone());
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
        let rpc = Rpc::new(cli, storage.clone());
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
}
