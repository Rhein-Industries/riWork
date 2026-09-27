use crate::{
    HANDSHAKE_SECONDS, MAX_FRAME,
    config::{Device, Storage},
    crypto::{
        ClientFinish, ClientHello, Envelope, Pending, Session, accept_hello, decode, random32,
    },
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
    connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
pub async fn connect_registered(
    relay: &str,
    route: &str,
    role: &str,
    token: &str,
) -> Result<(Socket, bool)> {
    let config = WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_FRAME * 2)
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    let (mut ws, _) = timeout(
        Duration::from_secs(10),
        connect_async_with_config(relay, Some(config), false),
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
    .await??;
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
    loop {
        match ws.next().await {
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
                for id in removed {if let Some((cancel,_))=running.remove(&id){let _=cancel.send(true);}}
                for (id,device) in active {
                    if let std::collections::hash_map::Entry::Vacant(entry) = running.entry(id) {
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
async fn supervise(device: Device, rpc: Arc<Rpc>, mut cancel: watch::Receiver<bool>) {
    loop {
        if *cancel.borrow() {
            return;
        }
        // Cancelling a pending input drops the CLI child but keeps the durable
        // pending ledger. Retry yields outcome_unknown instead of duplicate input.
        tokio::select! {
            _=cancel.changed()=>return,
            result=run_device(&device,&rpc)=>{
                if result.is_err(){eprintln!("Remote device {} disconnected; reconnecting (no payload logged).",device.pairing.device_id);}
            }
        }
        tokio::select! {_=cancel.changed()=>return,_=sleep(Duration::from_secs(1))=>{}}
    }
}
async fn run_device(device: &Device, rpc: &Rpc) -> Result<()> {
    let p = &device.pairing;
    p.validate(device.allow_insecure_loopback)?;
    let (mut ws, online) =
        connect_registered(&p.relay_url, &p.route_id, "desktop", &device.desktop_token).await?;
    let identity = p.identity();
    let secret = decode::<32>(&p.pairing_secret)?;
    let mut pending: Option<Pending> = None;
    let mut session: Option<Session> = None;
    let mut viewport: Option<Viewport> = None;
    let mut deadline: Option<Instant> =
        online.then(|| Instant::now() + Duration::from_secs(HANDSHAKE_SECONDS));
    let mut heartbeat = interval(Duration::from_secs(20));
    let mut renew = interval(Duration::from_secs(3));
    heartbeat.tick().await;
    loop {
        let value = tokio::select! {
            _=renew.tick()=>{if let Some(v)=&viewport {rpc.renew_viewport(v).await?;}continue;},
            _=heartbeat.tick()=>{ws.send(Message::Ping(vec![].into())).await?;continue;},
            _=async {if let Some(t)=deadline {tokio::time::sleep_until(t).await;}else{std::future::pending::<()>().await;}}=>bail!("handshake timeout"),
            value=receive_json(&mut ws)=>value?,
        };
        ensure!(value["v"] == 1, "unsupported version");
        match value["type"].as_str() {
            Some("peer") => {
                let online = value["online"].as_bool().context("missing peer status")?;
                // Ordered peer control messages reset old transport state.
                if let Some(mut v) = viewport.take() {
                    rpc.clear_viewport(&mut v).await?;
                }
                session = None;
                pending = None;
                deadline = online.then(|| Instant::now() + Duration::from_secs(HANDSHAKE_SECONDS));
            }
            Some("client_hello") => {
                ensure!(
                    session.is_none() && pending.is_none(),
                    "unexpected handshake restart"
                );
                ensure!(rpc.storage.authorized(&p.device_id)?, "revoked device");
                let hello: ClientHello = serde_json::from_value(value)?;
                let (reply, state) = accept_hello(&identity, &secret, &hello, random32())?;
                send_json(&mut ws, &reply).await?;
                pending = Some(state);
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
            }
            Some("encrypted") => {
                let s = session
                    .as_mut()
                    .context("RPC before authenticated handshake")?;
                let envelope: Envelope = serde_json::from_value(value)?;
                let plaintext = s.open("c2d", &envelope)?;
                let response = rpc
                    .handle_in(
                        &p.device_id,
                        serde_json::from_slice(&plaintext)?,
                        viewport.as_mut(),
                    )
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
