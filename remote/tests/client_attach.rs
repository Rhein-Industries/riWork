//! Attaching to a shell through the client daemon, against a host that speaks the
//! real v2 handshake and answers `pty.*` the way the host's design says
//! (`tests/client_support`). The daemon runs in this process; the bridge is played
//! by hand with the frame protocol. The real bridge, in a terminal, is in
//! `client_bridge.rs`.
mod client_support;

use client_support::{Bridge, FakeHost, HostOptions, Net, eventually, wait_for_daemon};
use riwork_remote::{
    client::{HOSTS_FILE, HostRecord, HostsFile, add_host},
    client_daemon::{
        DaemonTiming, daemon_call, daemon_status, read_frame, serve_with, socket_path,
    },
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
    setup_timed(options, DaemonTiming::default()).await
}
async fn setup_timed(options: HostOptions, timing: DaemonTiming) -> Setup {
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
        tokio::spawn(async move { serve_with(client, &id, Duration::from_secs(300), timing).await })
    };
    wait_for_daemon(&socket).await;
    // Wait for its link: the sessions the host counts from here are the daemon's.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let status = daemon_status(&socket).await.unwrap();
            if status["state"] == "online" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the daemon's link to the host comes online");
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

// ---- What a call that failed says about what happened ----------------------------------

#[tokio::test]
async fn a_call_cut_off_after_it_was_written_is_lost_and_one_made_while_down_was_not_sent() {
    let s = setup(HostOptions::default()).await;
    s.host.freeze(true);
    let call = {
        let socket = s.socket.clone();
        tokio::spawn(async move { daemon_call(&socket, "projects.list", json!({}), 20_000).await })
    };
    s.host
        .wait(5, "the call to arrive", |o| {
            o.frozen_calls.iter().any(|m| m == "projects.list")
        })
        .await;
    s.host.drop_connection(Duration::from_secs(30));
    let reply = tokio::time::timeout(Duration::from_secs(10), call)
        .await
        .expect("answered when the session ends")
        .unwrap()
        .unwrap();
    assert_eq!(reply["ok"], false, "{reply}");
    assert_eq!(reply["error"]["code"], "connection_lost", "{reply}");
    assert!(reply["error"]["message"].as_str().unwrap().len() > 3);
    assert!(reply["error"].get("not_sent").is_none(), "{reply}");

    tokio::time::timeout(Duration::from_secs(10), async {
        while daemon_status(&s.socket).await.unwrap()["state"] != "offline" {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the link says it is offline");
    let reply = daemon_call(&s.socket, "projects.list", json!({}), 5000)
        .await
        .unwrap();
    assert_eq!(reply["ok"], false, "{reply}");
    assert_eq!(reply["error"]["code"], "offline", "{reply}");
    assert_eq!(reply["error"]["not_sent"], true, "{reply}");
    // A plain outage has no cause to name, and time may yet fix it.
    assert!(reply["error"].get("cause").is_none(), "{reply}");
    assert!(reply["error"].get("needs_action").is_none(), "{reply}");
    assert_eq!(reply["server_ms"], 0, "{reply}");
}

#[tokio::test]
async fn an_error_from_the_host_cannot_claim_that_nothing_was_sent() {
    let s = setup(HostOptions::default()).await;
    let reply = daemon_call(&s.socket, "no.such.method", json!({}), 5000)
        .await
        .unwrap();
    assert_eq!(reply["error"]["code"], "invalid_request", "{reply}");
    assert!(reply["error"].get("not_sent").is_none(), "{reply}");
}

// ---- Opening terminals ------------------------------------------------------------------

/// The deadline of a `pty.open`, short enough to test.
fn opens_within(limit: Duration) -> DaemonTiming {
    DaemonTiming {
        open: limit,
        ..DaemonTiming::default()
    }
}

/// The bridge's statuses until it is online, within `seconds`. Before that it may only
/// hear that its terminal is on its way.
async fn online_in_the_end(bridge: &mut Bridge, seconds: u64) {
    tokio::time::timeout(Duration::from_secs(seconds), async {
        loop {
            let status = bridge.status().await;
            if status["state"] == "online" {
                return;
            }
            assert_eq!(status["state"], "connecting", "{status}");
        }
    })
    .await
    .expect("online in the end");
}

#[tokio::test]
async fn terminals_that_open_together_are_each_given_the_whole_deadline() {
    let s = setup_timed(HostOptions::default(), opens_within(Duration::from_secs(2))).await;
    // Each is quick enough, and the last would pass the deadline if the wait counted.
    s.host.open_delay(Duration::from_millis(800));
    let mut bridges = [s.attach().await, s.attach().await, s.attach().await];
    for bridge in &mut bridges {
        // A slow machine can keep the last waiting for its turn until it is told that its
        // terminal is on its way. What the deadline decides is whether an open is given up.
        online_in_the_end(bridge, 20).await;
        bridge.data_until(b"READY").await;
    }
    assert_eq!(s.host.seen(|o| o.opens_received), 3, "none was sent again");
    assert!(s.host.seen(|o| o.closes.is_empty()), "none was given up on");
    assert_eq!(
        s.host.seen(|o| o.max_opens_in_flight),
        1,
        "sent one at a time"
    );
    assert_eq!(s.host.seen(|o| o.sessions), 2, "the link was never reset");
}

#[tokio::test]
async fn an_open_the_host_is_slow_to_answer_costs_neither_the_link_nor_other_terminals() {
    // The answer comes a second after the deadline, and a second before the stream it
    // names is given up on: room either side for a slow machine.
    let s = setup_timed(HostOptions::default(), opens_within(Duration::from_secs(2))).await;
    let mut other = s.ready().await;
    s.host.open_delay(Duration::from_secs(3));
    let mut bridge = s.attach().await;
    s.host
        .wait(5, "the slow open to arrive", |o| o.opens_received == 2)
        .await;
    s.host.open_delay(Duration::ZERO);
    // The terminal is told at the deadline, and not that the host cannot be reached: the
    // link is fine. It comes up on the same session.
    assert_eq!(bridge.status().await["state"], "connecting");
    online_in_the_end(&mut bridge, 20).await;
    bridge.data_until(b"READY").await;
    bridge.data(b"typed").await;
    bridge.data_until(b"typed").await;
    // The stream the late answer named has nobody to read it, and is closed.
    s.host
        .wait(5, "the late stream to be closed", |o| !o.closes.is_empty())
        .await;
    assert_eq!(
        s.host.seen(|o| o.max_opens_in_flight),
        1,
        "the next open waits for the late answer"
    );
    assert_eq!(s.host.seen(|o| o.sessions), 2, "the link was never reset");
    other.data(b"still here").await;
    other.data_until(b"still here").await;
}

#[tokio::test]
async fn a_bridge_that_leaves_while_its_terminal_opens_leaves_no_stream_behind() {
    let s = setup(HostOptions::default()).await;
    s.host.open_delay(Duration::from_millis(200));
    // One more than the host's limit: each would keep its place for the life of the session.
    for left in 1..=9 {
        let bridge = s.attach().await;
        s.host
            .wait(5, "the open to arrive", |o| o.opens_received == left)
            .await;
        drop(bridge);
        s.host
            .wait(5, "its stream to be closed", |o| o.closes.len() == left)
            .await;
    }
    let mut bridge = s.ready().await;
    bridge.data(b"typed").await;
    bridge.data_until(b"typed").await;
    assert_eq!(s.host.seen(|o| o.sessions), 2, "the link was never reset");
}

#[tokio::test]
async fn the_next_terminal_waits_for_the_open_of_one_that_left() {
    let s = setup(HostOptions::default()).await;
    s.host.open_delay(Duration::from_millis(300));
    let gone = s.attach().await;
    s.host
        .wait(5, "the open to arrive", |o| o.opens_received == 1)
        .await;
    drop(gone);
    let mut next = s.attach().await;
    assert_eq!(next.status().await["state"], "online");
    next.data_until(b"READY").await;
    s.host
        .wait(5, "the first stream to be closed", |o| o.closes.len() == 1)
        .await;
    assert_eq!(s.host.seen(|o| o.max_opens_in_flight), 1);
}

#[tokio::test]
async fn a_terminal_that_leaves_while_waiting_its_turn_was_never_opened() {
    let s = setup(HostOptions::default()).await;
    // Long enough that the drop is seen first, however slow this machine is.
    s.host.open_delay(Duration::from_secs(2));
    let mut first = s.attach().await;
    s.host
        .wait(5, "the first open to arrive", |o| o.opens_received == 1)
        .await;
    let waiting = s.attach().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(waiting);
    assert_eq!(first.status().await["state"], "online");
    first.data_until(b"READY").await;
    // Its turn went to nobody, and nothing is left holding the way.
    let mut third = s.attach().await;
    assert_eq!(third.status().await["state"], "online");
    third.data_until(b"READY").await;
    assert_eq!(s.host.seen(|o| o.opens_received), 2);
    assert_eq!(s.host.seen(|o| o.closes.len()), 0);
}

#[tokio::test]
async fn a_terminal_waiting_its_turn_opens_at_the_size_it_has_by_then() {
    let s = setup(HostOptions::default()).await;
    // Long enough that the resize is seen first, however slow this machine is.
    s.host.open_delay(Duration::from_secs(2));
    let mut first = s.attach().await;
    s.host
        .wait(5, "the first open to arrive", |o| o.opens_received == 1)
        .await;
    let mut second = s.attach().await;
    second.resize(90, 20).await;
    assert_eq!(first.status().await["state"], "online");
    assert_eq!(second.status().await["state"], "online");
    let open = s.host.seen(|o| o.opens[1].clone());
    assert_eq!(
        (open["columns"].as_u64(), open["rows"].as_u64()),
        (Some(90), Some(20))
    );
}

#[tokio::test]
async fn a_terminal_kept_waiting_for_its_turn_past_the_deadline_is_told_and_still_opens() {
    // One open fits in the deadline and two do not, by more than a second either way.
    let s = setup_timed(
        HostOptions::default(),
        opens_within(Duration::from_millis(4400)),
    )
    .await;
    s.host.open_delay(Duration::from_secs(3));
    let _first = s.attach().await;
    s.host
        .wait(5, "the first open to arrive", |o| o.opens_received == 1)
        .await;
    let _second = s.attach().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut third = s.attach().await;
    let told = third.status().await;
    assert_eq!(told["state"], "connecting", "{told}");
    // Its own open has not gone out; on a slow machine the second's may not have either.
    assert!(
        s.host.seen(|o| o.opens_received) <= 2,
        "told while it was not yet its turn"
    );
    // Its own open need not wait as long.
    s.host
        .wait(10, "the second open to arrive", |o| o.opens_received == 2)
        .await;
    s.host.open_delay(Duration::ZERO);
    online_in_the_end(&mut third, 20).await;
    third.data_until(b"READY").await;
    assert_eq!(s.host.seen(|o| o.sessions), 2, "the link was never reset");
}

/// An open whose session ended before it was answered named no stream, so nothing is
/// closed, on that session or the next. That a stream which was named and then died with
/// its session is not closed on the next one is a unit test in `client_daemon`.
#[tokio::test]
async fn an_open_cut_off_with_its_session_asks_for_no_close() {
    let s = setup(HostOptions::default()).await;
    // Never answered: the session ends long before, however slow this machine is.
    s.host.open_delay(Duration::from_secs(5));
    let bridge = s.attach().await;
    s.host
        .wait(5, "the open to arrive", |o| o.opens_received == 1)
        .await;
    drop(bridge);
    tokio::time::sleep(Duration::from_millis(50)).await;
    s.host.drop_connection(Duration::from_millis(100));
    s.host
        .wait(10, "the next session", |o| o.sessions == 3)
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !s.host.seen(|o| o.calls.iter().any(|m| m == "pty.close")),
        "nothing to close, and nothing asked"
    );
}

// ---- The registry -----------------------------------------------------------------------

/// How often a daemon reads the registry, short enough to test.
fn a_quick_look_at_the_registry() -> DaemonTiming {
    DaemonTiming {
        registry: Duration::from_millis(100),
        ..DaemonTiming::default()
    }
}
impl Setup {
    fn registry_path(&self) -> PathBuf {
        self.client.dir.join(HOSTS_FILE)
    }
    fn record(&self) -> HostRecord {
        HostsFile::load(&self.client).unwrap().hosts.remove(0)
    }
    fn save(&self, hosts: Vec<HostRecord>) {
        HostsFile { v: 1, hosts }.save(&self.client).unwrap();
    }
    async fn daemon_quits(&self) {
        eventually(5, "the daemon to quit", || self.daemon.is_finished()).await;
        assert!(!self.socket.exists(), "the socket goes with it");
    }
    /// Long enough for several looks at the registry.
    async fn daemon_stays(&self) {
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!self.daemon.is_finished(), "the daemon quit");
        assert!(daemon_status(&self.socket).await.is_ok());
    }
}

#[tokio::test]
async fn a_daemon_whose_route_left_the_registry_quits_without_being_told() {
    let s = setup_timed(HostOptions::default(), a_quick_look_at_the_registry()).await;
    // A terminal is no reason to stay, and neither would it end by itself.
    let _bridge = s.ready().await;
    s.daemon_stays().await;
    s.save(vec![]);
    s.daemon_quits().await;
}

#[tokio::test]
async fn looking_at_the_registry_does_not_keep_an_idle_daemon_going() {
    let net = Net::new(1).await;
    let _host = FakeHost::start(&net, &net.invites[0].device_id, HostOptions::default());
    let client = Storage::at(net.client_home("client")).unwrap();
    add_host(&client, &net.link(0), Some("Studio"), true)
        .await
        .unwrap();
    let id = net.desktop_id();
    let served = serve_with(
        client,
        &id,
        Duration::from_secs(1),
        a_quick_look_at_the_registry(),
    );
    tokio::time::timeout(Duration::from_secs(5), served)
        .await
        .expect("it idles out")
        .unwrap();
}

#[tokio::test]
async fn a_daemon_whose_route_or_credentials_changed_quits() {
    let s = setup_timed(HostOptions::default(), a_quick_look_at_the_registry()).await;
    // The same id, as when a host is removed and paired again.
    let mut changed = s.record();
    changed.added_at_unix += 1;
    s.save(vec![changed]);
    s.daemon_quits().await;
}

#[tokio::test]
async fn a_new_name_for_a_route_does_not_interrupt_its_terminals() {
    let s = setup_timed(HostOptions::default(), a_quick_look_at_the_registry()).await;
    let mut bridge = s.ready().await;
    let mut renamed = s.record();
    renamed.label = "Renamed".into();
    s.save(vec![renamed]);
    s.daemon_stays().await;
    bridge.data(b"typed").await;
    bridge.data_until(b"typed").await;
}

/// A name is all that changed, so nothing is restarted, or told before it has to be told
/// something. But what the daemon says from then on uses the new one.
#[tokio::test]
async fn a_new_name_reaches_what_the_daemon_says_and_leaves_its_terminals_alone() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let s = setup_timed(HostOptions::default(), a_quick_look_at_the_registry()).await;
    let mut bridge = s.ready().await;
    let mut watching =
        tokio::io::BufReader::new(tokio::net::UnixStream::connect(&s.socket).await.unwrap());
    watching
        .get_mut()
        .write_all(b"{\"op\":\"watch\"}\n")
        .await
        .unwrap();
    let mut line = String::new();
    watching.read_line(&mut line).await.unwrap();
    let first: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(first["label"], "Studio", "{first}");

    let mut renamed = s.record();
    renamed.label = "Renamed".into();
    s.save(vec![renamed]);

    // Someone watching hears of it, as they would of any other change in the line.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            line.clear();
            watching.read_line(&mut line).await.unwrap();
            let status: Value = serde_json::from_str(&line).unwrap();
            if status["label"] == "Renamed" {
                break;
            }
        }
    })
    .await
    .expect("a line with the new name");
    let status = daemon_status(&s.socket).await.unwrap();
    assert_eq!(status["label"], "Renamed", "{status}");

    // A terminal has no reason to hear of it, and keeps going.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), read_frame(&mut bridge.stream))
            .await
            .is_err(),
        "nothing for a terminal"
    );
    bridge.data(b"typed").await;
    bridge.data_until(b"typed").await;
    assert_eq!(s.host.seen(|o| o.sessions), 2);
    assert_eq!(s.host.seen(|o| o.opens.len()), 1);

    // The next thing it is told uses the new name.
    s.host.drop_connection(Duration::from_millis(100));
    let offline = bridge.status().await;
    assert_eq!(offline["state"], "offline", "{offline}");
    assert_eq!(offline["label"], "Renamed", "{offline}");
    let online = bridge.status().await;
    assert_eq!(online["state"], "online", "{online}");
    assert_eq!(online["label"], "Renamed", "{online}");
}

/// Not a registry that says this route has gone: one nobody can look at right now. These
/// reach the next look through a rename, so that no look can find the file truly missing.
#[tokio::test]
async fn a_registry_that_cannot_be_looked_at_does_not_interrupt_a_healthy_terminal() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup_timed(HostOptions::default(), a_quick_look_at_the_registry()).await;
    let mut bridge = s.ready().await;
    let path = s.registry_path();
    let kept = s.client.dir.join("kept.json");
    std::fs::copy(&path, &kept).unwrap();
    let dangling = s.client.dir.join("dangling");
    std::os::unix::fs::symlink(s.client.dir.join("away.json"), &dangling).unwrap();
    std::fs::rename(&dangling, &path).unwrap();
    s.daemon_stays().await;
    std::fs::rename(&kept, &path).unwrap();
    s.daemon_stays().await;
    if unsafe { libc::geteuid() } != 0 {
        // The daemon's own socket is under here too, so it is asked only after this.
        std::fs::set_permissions(&s.client.dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        std::fs::set_permissions(&s.client.dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        s.daemon_stays().await;
    }
    bridge.data(b"typed").await;
    bridge.data_until(b"typed").await;
    assert_eq!(s.host.seen(|o| o.sessions), 2);
    // A file that really is gone still counts.
    std::fs::remove_file(&path).unwrap();
    s.daemon_quits().await;
}

#[tokio::test]
async fn a_damaged_registry_does_not_interrupt_a_healthy_terminal() {
    use std::os::unix::fs::PermissionsExt;
    let s = setup_timed(HostOptions::default(), a_quick_look_at_the_registry()).await;
    let mut bridge = s.ready().await;
    let path = s.registry_path();
    let good = std::fs::read(&path).unwrap();
    // Not JSON at all, ...
    std::fs::write(&path, b"{ not json").unwrap();
    s.daemon_stays().await;
    // ... a record that does not validate next to this one, ...
    let mut value: Value = serde_json::from_slice(&good).unwrap();
    let mut other = value["hosts"][0].clone();
    other["id"] = json!("not-a-uuid");
    value["hosts"].as_array_mut().unwrap().push(other);
    std::fs::write(&path, value.to_string()).unwrap();
    s.daemon_stays().await;
    // ... and a file others can read.
    std::fs::write(&path, &good).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    s.daemon_stays().await;
    bridge.data(b"typed").await;
    bridge.data_until(b"typed").await;
    assert_eq!(s.host.seen(|o| o.sessions), 2);
    // Repaired, it counts again.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    s.daemon_stays().await;
    s.save(vec![]);
    s.daemon_quits().await;
}
