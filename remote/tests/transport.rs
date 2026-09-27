use futures_util::{SinkExt, StreamExt};
use riwork_remote::{
    MAX_FRAME,
    connector::{connect_registered, receive_json},
    crypto::{b64, random32},
    relay::{Relay, Route, Routes},
};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::{Duration, timeout},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
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
    let d = random32();
    let m = random32();
    let route = uuid::Uuid::new_v4().to_string();
    let r = Relay::new(
        Routes {
            v: 1,
            routes: vec![Route {
                route_id: route.clone(),
                desktop_token_sha256: hex::encode(Sha256::digest(d)),
                mobile_token_sha256: hex::encode(Sha256::digest(m)),
            }],
        },
        limit,
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
#[tokio::test]
async fn global_socket_limit_applies_before_registration() {
    let s = server(1).await;
    let (first, _) = connect_async(&s.url).await.unwrap();
    assert!(connect_async(&s.url).await.is_err());
    drop(first);
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
