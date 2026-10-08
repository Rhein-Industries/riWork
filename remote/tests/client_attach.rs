//! Attaching to a shell through the client daemon, against a host that speaks the
//! real v2 handshake and answers `pty.*` the way the host's design says
//! (`tests/client_support`). The daemon runs in this process; the bridge is played
//! by hand with the frame protocol. The real bridge, in a terminal, is in
//! `client_bridge.rs`.
mod client_support;

use client_support::{Bridge, FakeHost, HostOptions, Net, wait_for_daemon};
use riwork_remote::{
    client::add_host,
    client_daemon::{daemon_call, serve, socket_path},
    config::Storage,
};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio::task::JoinHandle;

struct Setup {
    net: Net,
    host: FakeHost,
    client: Storage,
    socket: PathBuf,
    id: String,
    shell: String,
    daemon: JoinHandle<anyhow::Result<()>>,
}
impl Drop for Setup {
    fn drop(&mut self) {
        self.daemon.abort();
    }
}
async fn setup(options: HostOptions) -> Setup {
    let net = Net::new(1).await;
    let id = net.desktop_id();
    let host = FakeHost::start(&net, &net.invites[0].device_id, options);
    let client = Storage::at(net.client_home("client")).unwrap();
    let added = add_host(&client, &net.link(0), Some("Studio"), true)
        .await
        .unwrap();
    assert!(added.warning.is_none(), "{:?}", added.warning);
    let socket = socket_path(&client, &id).unwrap();
    let daemon = {
        let (client, id) = (client.clone(), id.clone());
        tokio::spawn(async move { serve(client, &id, Duration::from_secs(300)).await })
    };
    wait_for_daemon(&socket).await;
    // Wait for its link: the sessions the host counts from here are the daemon's.
    loop {
        let status = riwork_remote::client_daemon::daemon_status(&socket)
            .await
            .unwrap();
        if status["state"] == "online" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Setup {
        net,
        host,
        client,
        socket,
        id,
        shell: uuid::Uuid::new_v4().to_string(),
        daemon,
    }
}
impl Setup {
    async fn attach(&self) -> Bridge {
        Bridge::attach(&self.socket, &self.shell, 100, 30).await
    }
    /// An attached bridge that has seen its stream come up.
    async fn ready(&self) -> Bridge {
        let mut bridge = self.attach().await;
        assert_eq!(bridge.status().await["state"], "online");
        bridge.data_until(b"READY").await;
        bridge
    }
}

#[tokio::test]
async fn a_stream_echoes_what_is_typed_and_writes_arrive_in_order_with_their_gaps() {
    let s = setup(HostOptions::default()).await;
    let mut bridge = s.ready().await;
    let open = s.host.seen(|o| o.opens[0].clone());
    assert_eq!(open["shell_id"], s.shell.as_str());
    assert_eq!(
        (open["columns"].as_u64(), open["rows"].as_u64()),
        (Some(100), Some(30))
    );
    assert_eq!(open["term"], "xterm-256color");
    assert_eq!(open["ignore_size"], false);

    bridge.data(b"ls").await;
    bridge.data_until(b"ls").await;
    bridge.data(b"\r").await;
    bridge.data_until(b"\r").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    bridge.data(b"\r").await;
    bridge.data_until(b"\r").await;
    let writes = s.host.seen(|o| o.writes.clone());
    assert_eq!(
        writes.iter().map(|w| w.data.clone()).collect::<Vec<_>>(),
        vec![b"ls".to_vec(), b"\r".to_vec(), b"\r".to_vec()]
    );
    // The sequence is the running byte count, never a gap or a repeat.
    assert_eq!(
        writes.iter().map(|w| w.seq).collect::<Vec<_>>(),
        vec![0, 2, 3]
    );
    // Writes sent as typed are as far apart as the person made them, so no pause is asked
    // of the host: a Return after a wait costs no delay.
    assert!(writes.iter().all(|w| w.gap_ms == 0), "{writes:?}");
    // One read is parked per stream: the relay drops a socket whose queue passes 16 messages.
    assert_eq!(s.host.seen(|o| o.max_parked_reads), 1);

    bridge.resize(120, 40).await;
    s.host
        .wait(5, "the resize", |o| o.resizes == vec![(120, 40)])
        .await;

    // The host ends the stream: the bridge gets the rest of the output, then the reason.
    s.host.output(b"bye");
    s.host.end("exited");
    let rest = bridge.data_until(b"bye").await;
    assert!(rest.ends_with(b"bye"));
    let (kind, payload) = bridge.next().await.unwrap();
    assert_eq!(kind, b'E');
    assert_eq!(
        serde_json::from_slice::<Value>(&payload).unwrap(),
        json!({"reason":"exited"})
    );
    assert!(
        bridge.next().await.is_none(),
        "the daemon hangs up after the end"
    );
    // Its slot on the host is released too.
    s.host.wait(5, "pty.close", |o| o.closes.len() == 1).await;
}

#[tokio::test]
async fn a_paste_is_cut_at_the_hosts_write_limit_and_arrives_whole() {
    let s = setup(HostOptions::default()).await;
    let mut bridge = s.ready().await;
    let paste: Vec<u8> = (0..300_000u32).map(|i| b'a' + (i % 26) as u8).collect();
    // The bridge sends what the terminal gives it, in pieces of its own size.
    for piece in paste.chunks(60_000) {
        bridge.data(piece).await;
    }
    s.host
        .wait(20, "the whole paste", |o| o.typed().len() == 300_000)
        .await;
    let (typed, writes) = s.host.seen(|o| (o.typed(), o.writes.clone()));
    assert_eq!(typed, paste);
    assert!(
        writes
            .iter()
            .all(|w| w.data.len() <= 32_768 && w.gap_ms == 0)
    );
    let mut next = 0;
    for write in &writes {
        assert_eq!(write.seq, next, "writes arrive in order");
        next += write.data.len() as u64;
    }
    // And it comes back (the host echoes it), in order.
    let mut echoed = Vec::new();
    while echoed.len() < paste.len() {
        let (kind, payload) = bridge.next().await.unwrap();
        if kind == b'D' {
            echoed.extend(payload);
        }
    }
    assert_eq!(echoed, paste);
    // Big replies came compressed, and were read.
    assert!(
        s.host.seen(|o| o.deflated_replies) > 0,
        "the host deflated its big replies"
    );
}

#[tokio::test]
async fn a_lost_link_is_announced_and_the_stream_is_opened_again_without_replaying_keys() {
    let s = setup(HostOptions::default()).await;
    let mut bridge = s.ready().await;
    bridge.data(b"before").await;
    bridge.data_until(b"before").await;

    s.host.drop_connection(Duration::from_millis(800));
    let offline = bridge.status().await;
    assert_eq!(offline["state"], "offline", "{offline}");
    assert!(offline["reason"].as_str().is_some_and(|r| !r.is_empty()));
    assert_eq!(offline["label"], "Studio");
    // Typed while the link is down, and a window change, which the new stream must use.
    bridge.data(b"LOST").await;
    bridge.resize(90, 20).await;

    let online = bridge.status().await;
    assert_eq!(online["state"], "online", "{online}");
    bridge.data_until(b"READY").await;
    s.host
        .wait(5, "a second stream", |o| o.opens.len() == 2)
        .await;
    let (opens, sessions) = s.host.seen(|o| (o.opens.clone(), o.sessions));
    // `hosts add`'s check, the daemon's first session, and this one.
    assert_eq!(sessions, 3);
    assert_ne!(opens[0], opens[1].clone(), "the sizes differ");
    assert_eq!(
        (opens[1]["columns"].as_u64(), opens[1]["rows"].as_u64()),
        (Some(90), Some(20))
    );
    assert_eq!(opens[1]["shell_id"], s.shell.as_str());

    // The stream works again, from the start of its own sequence; nothing typed
    // during the outage reached the host.
    bridge.data(b"after").await;
    bridge.data_until(b"after").await;
    let writes = s.host.seen(|o| o.writes.clone());
    assert_eq!(
        writes.iter().map(|w| w.data.clone()).collect::<Vec<_>>(),
        vec![b"before".to_vec(), b"after".to_vec()]
    );
    assert_eq!(writes[1].seq, 0);
    assert_ne!(writes[0].stream, writes[1].stream);
}

#[tokio::test]
async fn keys_waiting_to_be_written_when_the_link_drops_are_not_written_to_the_next_stream() {
    let s = setup(HostOptions::default()).await;
    let mut bridge = s.ready().await;
    // The host stops answering, so the first writes stay in flight and the rest wait.
    s.host.freeze(true);
    for key in ["a", "b", "c", "d", "e", "f", "g", "h"] {
        bridge.data(key.as_bytes()).await;
    }
    // Four writes are in flight, unanswered; the rest of the keys are queued behind them.
    s.host
        .wait(5, "four writes in flight", |o| o.frozen_writes == 4)
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    s.host.drop_connection(Duration::from_millis(200));
    assert_eq!(bridge.status().await["state"], "offline");
    assert_eq!(bridge.status().await["state"], "online");
    bridge.data_until(b"READY").await;
    bridge.data(b"z").await;
    bridge.data_until(b"z").await;
    assert_eq!(
        s.host.seen(|o| o.typed()),
        b"z",
        "nothing from before the drop"
    );
}

#[tokio::test]
async fn a_link_that_comes_back_at_once_still_tells_the_bridge_to_reset() {
    let s = setup(HostOptions::default()).await;
    let mut bridge = s.ready().await;
    s.host.drop_connection(Duration::ZERO);
    // However quickly the host returns, the screen was interrupted: offline, then online.
    assert_eq!(bridge.status().await["state"], "offline");
    assert_eq!(bridge.status().await["state"], "online");
    bridge.data_until(b"READY").await;
    assert_eq!(s.host.seen(|o| o.opens.len()), 2);
}

#[tokio::test]
async fn an_attach_that_cannot_work_ends_with_a_reason_the_person_can_act_on() {
    // A host that offers no terminal streams (a phone's pairing).
    let s = setup(HostOptions {
        pty: None,
        ..HostOptions::default()
    })
    .await;
    let mut bridge = s.attach().await;
    let end = last_end(&mut bridge).await;
    assert!(
        end.contains("does not offer terminal streams") && end.contains("--kind desktop"),
        "{end}"
    );

    for (code, message, expect) in [
        (
            "invalid_request",
            "unsupported RPC method",
            "--kind desktop",
        ),
        ("not_found", "no such shell", "not_found: no such shell"),
        (
            "pty_limit",
            "too many terminals",
            "pty_limit: too many terminals",
        ),
    ] {
        let s = setup(HostOptions {
            refuse_open: Some((code, message)),
            ..HostOptions::default()
        })
        .await;
        let mut bridge = s.attach().await;
        let end = last_end(&mut bridge).await;
        assert!(end.contains(expect), "{code}: {end}");
    }
    // A request that is not one is answered, not obeyed.
    let s = setup(HostOptions::default()).await;
    let request = riwork_remote::client_daemon::AttachRequest {
        shell_id: "not-a-uuid".into(),
        columns: 80,
        rows: 24,
        term: "xterm-256color".into(),
        ignore_size: false,
    };
    let mut stream = riwork_remote::client_daemon::daemon_attach(&s.socket, &request)
        .await
        .unwrap();
    let (kind, payload) = riwork_remote::client_daemon::read_frame(&mut stream)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kind, b'E');
    assert!(String::from_utf8_lossy(&payload).contains("invalid attach request"));
}

async fn last_end(bridge: &mut Bridge) -> String {
    loop {
        let (kind, payload) = bridge.next().await.expect("an end before the hangup");
        if kind == b'E' {
            return serde_json::from_slice::<Value>(&payload).unwrap()["reason"]
                .as_str()
                .unwrap()
                .to_owned();
        }
    }
}

#[tokio::test]
async fn a_bridge_that_goes_away_closes_its_stream_on_the_host() {
    let s = setup(HostOptions::default()).await;
    let bridge = s.ready().await;
    let stream = s.host.seen(|o| o.opens.len());
    assert_eq!(stream, 1);
    drop(bridge);
    s.host.wait(10, "pty.close", |o| o.closes.len() == 1).await;
}

#[tokio::test]
async fn as_many_terminals_as_the_host_allows_all_get_output_over_one_link() {
    let s = setup(HostOptions::default()).await;
    // `hosts add` made its own connection to check the host; the daemon's is the second.
    assert_eq!(s.host.seen(|o| o.sessions), 2);
    let mut bridges = Vec::new();
    for _ in 0..8 {
        bridges.push(s.ready().await);
    }
    for (i, bridge) in bridges.iter_mut().enumerate() {
        let word = format!("term{i}");
        bridge.data(word.as_bytes()).await;
        bridge.data_until(word.as_bytes()).await;
    }
    let reply = daemon_call(&s.socket, "projects.list", json!({}), 5000)
        .await
        .unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"], json!({"projects":[]}));
    assert!(reply["server_ms"].is_u64());
    assert!(s.host.seen(|o| o.max_parked_reads) <= 12);
    assert_eq!(
        s.host.seen(|o| o.sessions),
        2,
        "one connection for all of it"
    );
    assert_eq!(s.host.seen(|o| o.opens.len()), 8);
}

#[tokio::test]
async fn a_terminal_that_finds_no_read_free_waits_for_one_instead_of_stalling_the_others() {
    let pty = json!({"max_streams":8,"max_reads":2,"max_write":32768,"max_chunk":65536});
    let s = setup(HostOptions {
        pty: Some(pty),
        ..HostOptions::default()
    })
    .await;
    let mut a = s.ready().await;
    let mut b = s.ready().await;
    // The third has its stream, but no read to see it with: it is not announced, and it
    // does not hold the others up, until one of them ends.
    let mut c = s.attach().await;
    a.data(b"one").await;
    a.data_until(b"one").await;
    b.data(b"two").await;
    b.data_until(b"two").await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(400),
            riwork_remote::client_daemon::read_frame(&mut c.stream)
        )
        .await
        .is_err(),
        "nothing for a terminal that cannot read yet"
    );
    drop(a);
    assert_eq!(c.status().await["state"], "online");
    c.data_until(b"READY").await;
    c.data(b"three").await;
    c.data_until(b"three").await;
}

#[tokio::test]
async fn window_changes_are_folded_into_the_latest_size() {
    let s = setup(HostOptions::default()).await;
    let mut bridge = s.ready().await;
    for columns in 101..=300 {
        bridge.resize(columns, 40).await;
    }
    s.host
        .wait(10, "the last size", |o| {
            o.resizes.last() == Some(&(300, 40))
        })
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let resizes = s.host.seen(|o| o.resizes.clone());
    assert_eq!(resizes.last(), Some(&(300, 40)));
    assert!(
        resizes.len() < 100,
        "{} requests for 200 changes",
        resizes.len()
    );
    assert!(
        resizes.windows(2).all(|w| w[0].0 < w[1].0),
        "never out of order"
    );
}

#[tokio::test]
async fn a_misbehaving_client_cannot_hurt_the_daemon() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let s = setup(HostOptions::default()).await;
    let connect = || tokio::net::UnixStream::connect(&s.socket);

    let mut junk = connect().await.unwrap();
    junk.write_all(b"this is not json\n").await.unwrap();
    let mut answer = String::new();
    junk.read_to_string(&mut answer).await.unwrap();
    let reply: Value = serde_json::from_str(answer.lines().next().unwrap()).unwrap();
    assert_eq!(reply["error"]["code"], "invalid_request");

    // A line without end is cut off at the limit, and only that connection suffers.
    let mut endless = connect().await.unwrap();
    let chunk = vec![b'x'; 256 * 1024];
    for _ in 0..8 {
        if endless.write_all(&chunk).await.is_err() {
            break;
        }
    }
    let mut sink = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), endless.read_to_end(&mut sink)).await;

    // A frame that claims to be larger than allowed ends that attach.
    let mut bridge = s.ready().await;
    let mut header = vec![b'D'];
    header.extend_from_slice(&u32::MAX.to_be_bytes());
    bridge.stream.write_all(&header).await.unwrap();
    while let Ok(Some(_)) = tokio::time::timeout(
        Duration::from_secs(10),
        riwork_remote::client_daemon::read_frame(&mut bridge.stream),
    )
    .await
    .unwrap()
    {}
    // The daemon is still serving, and a call from an unknown op is told so.
    let reply = daemon_call(&s.socket, "projects.list", json!({}), 5000)
        .await
        .unwrap();
    assert_eq!(reply["ok"], true);
    let mut unknown = connect().await.unwrap();
    unknown.write_all(b"{\"op\":\"reboot\"}\n").await.unwrap();
    let mut answer = String::new();
    let mut lines = tokio::io::BufReader::new(unknown);
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::io::AsyncBufReadExt::read_line(&mut lines, &mut answer),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(answer.contains("unknown op"), "{answer}");
    let _ = (&s.id, &s.client, &s.net);
}
