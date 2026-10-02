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
    let LinkState::Offline { since, reason } = gone else {
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
async fn requests_in_flight_when_the_session_ends_fail_as_offline_and_a_new_session_follows() {
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
    assert!(matches!(error, CallError::Offline(_)), "{error:?}");
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
