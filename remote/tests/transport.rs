use futures_util::{SinkExt, StreamExt};
use riwork_remote::{
    MAX_FRAME,
    connector::{connect_registered, receive_json},
    crypto::{b64, random32},
    relay::{Relay, Route, Routes, Tuning},
};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::{Duration, timeout},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
struct Server {
    url: String,
    addr: std::net::SocketAddr,
    route: String,
    desktop: String,
    mobile: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server(limit: usize) -> Server {
    server_with(limit, Tuning::default()).await
}
async fn server_with(limit: usize, tuning: Tuning) -> Server {
    let d = random32();
    let m = random32();
    let route = uuid::Uuid::new_v4().to_string();
    let r = Relay::with_tuning(
        Routes {
            v: 1,
            routes: vec![Route {
                route_id: route.clone(),
                desktop_token_sha256: hex::encode(Sha256::digest(d)),
                mobile_token_sha256: hex::encode(Sha256::digest(m)),
            }],
        },
        limit,
        tuning,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, r.router()).await.unwrap();
    });
    Server {
        url: format!("ws://{addr}/v1/ws"),
        addr,
        route,
        desktop: b64(&d),
        mobile: b64(&m),
        task,
    }
}
#[tokio::test]
async fn opaque_forwarding_peer_notifications_health_and_duplicate_rejection() {
    let s = server(16).await;
    let mut http = tokio::net::TcpStream::connect(s.addr).await.unwrap();
    http.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut body = String::new();
    http.read_to_string(&mut body).await.unwrap();
    assert!(body.contains("200 OK") && body.ends_with("ok\n"));
    let (mut d, online) = connect_registered(&s.url, &s.route, "desktop", &s.desktop)
        .await
        .unwrap();
    assert!(!online);
    let (mut m, online) = connect_registered(&s.url, &s.route, "mobile", &s.mobile)
        .await
        .unwrap();
    assert!(online);
    assert_eq!(receive_json(&mut d).await.unwrap()["online"], true);
    assert!(
        connect_registered(&s.url, &s.route, "mobile", &s.mobile)
            .await
            .is_err()
    );
    let opaque = "not JSON; content is opaque and never inspected";
    m.send(Message::Text(opaque.into())).await.unwrap();
    assert_eq!(
        d.next().await.unwrap().unwrap().into_text().unwrap(),
        opaque
    );
    m.close(None).await.unwrap();
    assert_eq!(receive_json(&mut d).await.unwrap()["online"], false);
}
#[tokio::test]
async fn bad_route_role_token_binary_and_oversized_frames_close() {
    let s = server(16).await;
    assert!(
        connect_registered(&s.url, &s.route, "desktop", &s.mobile)
            .await
            .is_err()
    );
    assert!(
        connect_registered(
            &s.url,
            &uuid::Uuid::new_v4().to_string(),
            "desktop",
            &s.desktop
        )
        .await
        .is_err()
    );
    assert!(
        connect_registered(&s.url, &s.route, "invalid", &s.desktop)
            .await
            .is_err()
    );
    let (mut d, _) = connect_registered(&s.url, &s.route, "desktop", &s.desktop)
        .await
        .unwrap();
    let (mut m, _) = connect_registered(&s.url, &s.route, "mobile", &s.mobile)
        .await
        .unwrap();
    let _ = receive_json(&mut d).await.unwrap();
    m.send(Message::Binary(vec![1, 2].into())).await.unwrap();
    assert!(
        timeout(Duration::from_secs(2), receive_json(&mut m))
            .await
            .unwrap()
            .is_err()
    );
    let _ = receive_json(&mut d).await.unwrap();
    let (mut m, _) = connect_registered(&s.url, &s.route, "mobile", &s.mobile)
        .await
        .unwrap();
    let _ = receive_json(&mut d).await.unwrap();
    let _ = m
        .send(Message::Text("a".repeat(MAX_FRAME + 1).into()))
        .await;
    assert!(
        timeout(Duration::from_secs(2), receive_json(&mut m))
            .await
            .unwrap()
            .is_err()
    );
}
/// True once the relay has ended this client socket (close frame, error or EOF).
async fn ended(ws: &mut Client, within: Duration) -> bool {
    timeout(within, async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .is_ok()
}
#[tokio::test]
async fn authenticated_socket_limit_applies_at_registration_and_frees_on_close() {
    let s = server(1).await;
    let (first, _) = connect_registered(&s.url, &s.route, "desktop", &s.desktop)
        .await
        .unwrap();
    assert!(
        connect_registered(&s.url, &s.route, "mobile", &s.mobile)
            .await
            .is_err()
    );
    drop(first);
    let end = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if connect_registered(&s.url, &s.route, "mobile", &s.mobile)
            .await
            .is_ok()
        {
            break;
        }
        assert!(tokio::time::Instant::now() < end, "slot was never released");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
#[tokio::test]
async fn silent_unauthenticated_sockets_cannot_starve_registration() {
    // Two authenticated slots and a registration window far longer than the
    // test: only eviction from the separate pre-auth budget can make room.
    let s = server_with(
        2,
        Tuning {
            preauth_sockets: 4,
            register_timeout: Duration::from_secs(60),
            ..Tuning::default()
        },
    )
    .await;
    let mut silent = vec![];
    for _ in 0..12 {
        let (ws, _) = connect_async(&s.url).await.unwrap();
        silent.push(ws);
    }
    let (_d, _) = timeout(
        Duration::from_secs(2),
        connect_registered(&s.url, &s.route, "desktop", &s.desktop),
    )
    .await
    .unwrap()
    .unwrap();
    let (_m, online) = timeout(
        Duration::from_secs(2),
        connect_registered(&s.url, &s.route, "mobile", &s.mobile),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(online);
    // The budget was enforced by dropping the oldest silent sockets.
    assert!(ended(&mut silent[0], Duration::from_secs(2)).await);
}
#[tokio::test]
async fn unauthenticated_socket_is_dropped_after_the_registration_window() {
    let s = server_with(
        4,
        Tuning {
            register_timeout: Duration::from_millis(200),
            ..Tuning::default()
        },
    )
    .await;
    let (mut silent, _) = connect_async(&s.url).await.unwrap();
    assert!(ended(&mut silent, Duration::from_secs(3)).await);
}
fn quick_liveness() -> Tuning {
    Tuning {
        ping_interval: Duration::from_millis(100),
        idle_timeout: Duration::from_millis(800),
        replace_after: Duration::from_secs(60),
        ..Tuning::default()
    }
}
#[tokio::test]
#[ignore = "slow: about 3 s of liveness windows against a real relay"]
async fn relay_closes_silent_registrations_but_keeps_responsive_ones() {
    let s = server_with(8, quick_liveness()).await;
    // Held but never polled: no pong is ever sent for the relay's pings.
    let (_silent_desktop, _) = connect_registered(&s.url, &s.route, "desktop", &s.desktop)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1400)).await;
    let (mut mobile, online) = connect_registered(&s.url, &s.route, "mobile", &s.mobile)
        .await
        .unwrap();
    assert!(!online, "the silent desktop registration must have expired");
    // A responsive peer answers pings (tungstenite pongs while it is polled).
    let (mut desktop, _) = connect_registered(&s.url, &s.route, "desktop", &s.desktop)
        .await
        .unwrap();
    assert_eq!(receive_json(&mut mobile).await.unwrap()["online"], true);
    let responsive = tokio::spawn(async move { while desktop.next().await.is_some() {} });
    // Long past the 800 ms idle timeout only liveness traffic flows: no
    // peer-offline notification or close may arrive at the (also polled) mobile.
    assert!(
        timeout(Duration::from_millis(1600), receive_json(&mut mobile))
            .await
            .is_err(),
        "responsive registrations must not be expired"
    );
    responsive.abort();
}
#[tokio::test]
async fn silent_registration_is_replaced_by_the_same_credentials_only() {
    let s = server_with(
        8,
        Tuning {
            ping_interval: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(60),
            replace_after: Duration::from_secs(1),
            ..Tuning::default()
        },
    )
    .await;
    let (mut desktop, _) = connect_registered(&s.url, &s.route, "desktop", &s.desktop)
        .await
        .unwrap();
    let (mut stale, _) = connect_registered(&s.url, &s.route, "mobile", &s.mobile)
        .await
        .unwrap();
    assert_eq!(receive_json(&mut desktop).await.unwrap()["online"], true);
    // A live registration is still a duplicate.
    assert!(
        connect_registered(&s.url, &s.route, "mobile", &s.mobile)
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    // Silent, but wrong credentials must not displace it.
    assert!(
        connect_registered(&s.url, &s.route, "mobile", &s.desktop)
            .await
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(100), receive_json(&mut desktop))
            .await
            .is_err(),
        "an unauthenticated attempt must not disturb the registered peer"
    );
    let (_fresh, online) = connect_registered(&s.url, &s.route, "mobile", &s.mobile)
        .await
        .unwrap();
    assert!(online);
    // The peer sees the old one leave before the new one arrives.
    assert_eq!(receive_json(&mut desktop).await.unwrap()["online"], false);
    assert_eq!(receive_json(&mut desktop).await.unwrap()["online"], true);
    assert!(
        ended(&mut stale, Duration::from_secs(2)).await,
        "the replaced socket must be closed"
    );
}
#[tokio::test]
async fn peer_absence_does_not_buffer_content() {
    let s = server(16).await;
    let (mut m, _) = connect_registered(&s.url, &s.route, "mobile", &s.mobile)
        .await
        .unwrap();
    m.send(Message::Text("opaque".into())).await.unwrap();
    assert!(
        timeout(Duration::from_secs(2), receive_json(&mut m))
            .await
            .unwrap()
            .is_err()
    );
}
