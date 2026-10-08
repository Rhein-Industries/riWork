//! The chat RPCs end to end: a real relay and the real connector binary, a stand-in
//! CLI that prints canned chat pages, and a phone that speaks the frames. `ready`
//! announces chats when the CLI has them; a wait for events is a long poll that
//! leaves room for typing and reads; a page of events is sealed as the session
//! asked, and fits one frame.
use riwork_remote::{
    MAX_PLAINTEXT,
    config::{Pairing, Storage, private_read},
    connector::{connect_registered, receive_json, send_json},
    crypto::{Envelope, ServerHello, Session, accept_server, client_hello, decode, random32},
    link,
    relay::{Relay, Routes},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Instant,
};
use tokio::{
    process::{Child, Command},
    time::{Duration, sleep, timeout},
};
use uuid::Uuid;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Rig {
    dir: tempfile::TempDir,
    pairing: Pairing,
    chat: String,
    connector: Child,
    relay: tokio::task::JoinHandle<()>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.connector.start_kill();
        self.relay.abort();
    }
}

/// A CLI that logs every call in `calls.log` and answers the chat commands: `capabilities`
/// from `capabilities.out` (default: chats are there; `capabilities.unknown` makes it a CLI
/// from before the question), `chat list` with no chats, `chat events` with the page in
/// `events.json` after the seconds in `events.delay` (if that exists), `chat command` and
/// `chat stop` with the chat they were given, and `chat new` after `create.delay` seconds,
/// marking `create.ran`, with `create.json`. `orchestrator create` waits `orchestrator.delay`
/// seconds if that exists, marks `orchestrator.ran` and prints `orchestrator.json`.
fn stand_in_cli(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let cli = dir.join("canned-riwork");
    let script = r#"#!/bin/sh
D='@DIR@'
[ "$1" = warm ] && exit 0
echo "$*" >> "$D/calls.log"
case "$1 $2" in
'capabilities --json')
  if [ -e "$D/capabilities.unknown" ]; then echo "riwork: Unknown invocation 'capabilities'" >&2; exit 2; fi
  if [ -e "$D/capabilities.out" ]; then cat "$D/capabilities.out"; else printf '{"v":1,"chat":true,"orchestrator_create":true,"shell_create_as_settings":true,"chat_provider_switch":true,"chat_models":true}'; fi;;
'project show') printf '{"id":"%s"}' "$3";;
'chat list') echo '[]';;
'chat events')
  if [ -e "$D/events.delay" ]; then sleep "$(cat "$D/events.delay")"; fi
  cat "$D/events.json";;
'chat command') printf '{"id":"%s","status":"ok"}' "$3";;
'chat stop') printf '{"id":"%s","state":"stopped"}' "$3";;
'chat new')
  if [ -e "$D/create.delay" ]; then sleep "$(cat "$D/create.delay")"; fi
  touch "$D/create.ran"
  cat "$D/create.json";;
'orchestrator create')
  if [ -e "$D/orchestrator.delay" ]; then sleep "$(cat "$D/orchestrator.delay")"; fi
  touch "$D/orchestrator.ran"
  cat "$D/orchestrator.json";;
esac
"#
    .replace("@DIR@", &dir.to_string_lossy());
    std::fs::write(&cli, script).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}

impl Rig {
    async fn new() -> Self {
        Self::with(|_| {}).await
    }
    /// `prepare` sets the stand-in's files before the connector starts.
    async fn with(prepare: impl FnOnce(&Path)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        prepare(dir.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
        let storage = Storage::at(home.clone()).unwrap();
        let routes_path = dir.path().join("routes.json");
        let pairing = storage
            .pair(
                url,
                "chat test".into(),
                true,
                &dir.path().join("pair.json"),
                Some(&routes_path),
            )
            .unwrap();
        let routes: Routes = private_read(&routes_path, 1_048_576).unwrap();
        let relay = Relay::new(routes, 16).unwrap();
        let relay = tokio::spawn(async move {
            axum::serve(listener, relay.router()).await.unwrap();
        });
        let cli = stand_in_cli(dir.path());
        // The first run of a new executable can take seconds on a busy Mac; `ready` waits
        // for the CLI only briefly.
        let _ = std::process::Command::new(&cli).arg("warm").output();
        let connector = Command::new(env!("CARGO_BIN_EXE_riwork-remote"))
            .env("RIWORK_HOME", &home)
            .args(["start", "--riwork"])
            .arg(&cli)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            dir,
            pairing,
            chat: Uuid::new_v4().to_string(),
            connector,
            relay,
        }
    }
    fn canned(&self, name: &str, value: &Value) {
        std::fs::write(self.dir.path().join(name), value.to_string()).unwrap();
    }
    fn calls(&self, first: &str, second: &str) -> usize {
        std::fs::read_to_string(self.dir.path().join("calls.log"))
            .unwrap_or_default()
            .lines()
            .filter(|line| {
                let mut words = line.split(' ');
                words.next() == Some(first) && words.next() == Some(second)
            })
            .count()
    }
    /// A page of `count` events of `text` each, from seq 1.
    fn events(&self, count: u64, text: impl Fn(u64) -> String) {
        let events: Vec<Value> = (1..=count)
            .map(|seq| {
                json!({"seq": seq, "event": {"event": "item_completed", "item": {
                    "id": format!("agent-{seq}"), "status": "completed",
                    "body": {"type": "agent_message", "text": text(seq)}}}})
            })
            .collect();
        self.canned(
            "events.json",
            &json!({"chat_id": self.chat, "events": events, "next": count, "more": false}),
        );
    }
}

/// Text no deflate can shrink: a xorshift stream over the printable ASCII range.
fn noise(bytes: usize, seed: u64) -> String {
    let mut x = 0x2545_F491_4F6C_DD1Du64 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (0..bytes)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'!' + (x % 90) as u8)
        })
        .filter(|c| *c != '"' && *c != '\\')
        .collect()
}

struct Phone {
    ws: Socket,
    session: Session,
    ready: Value,
}
impl Phone {
    async fn connect(pair: &Pairing) -> Self {
        let end = tokio::time::Instant::now() + Duration::from_secs(10);
        let (mut ws, online) = loop {
            match connect_registered(&pair.relay_url, &pair.route_id, "mobile", &pair.relay_token)
                .await
            {
                Ok(s) => break s,
                Err(e) => {
                    assert!(tokio::time::Instant::now() < end, "relay: {e:#}");
                    sleep(Duration::from_millis(50)).await;
                }
            }
        };
        if !online {
            let peer = timeout(Duration::from_secs(10), receive_json(&mut ws))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(peer["online"], true);
        }
        let nonce = random32();
        let secret = decode::<32>(&pair.pairing_secret).unwrap();
        send_json(
            &mut ws,
            &client_hello(&pair.identity(), &secret, nonce).unwrap(),
        )
        .await
        .unwrap();
        let server: ServerHello =
            serde_json::from_value(receive_json(&mut ws).await.unwrap()).unwrap();
        let (finish, mut session) =
            accept_server(&pair.identity(), &secret, &nonce, &server).unwrap();
        send_json(&mut ws, &finish).await.unwrap();
        let ready: Envelope = serde_json::from_value(receive_json(&mut ws).await.unwrap()).unwrap();
        let ready: Value = serde_json::from_slice(&session.open("d2c", &ready).unwrap()).unwrap();
        assert_eq!(ready["type"], "ready");
        Self { ws, session, ready }
    }
    /// The reply to a request: the sealed plaintext's first byte, the JSON it holds,
    /// and the size of the frame on the socket.
    async fn call(&mut self, method: &str, params: Value) -> Reply {
        let id = self.send(method, params).await;
        let reply = self.next().await;
        assert_eq!(reply.value["id"], id, "{}", reply.value);
        reply
    }
    /// Sends a request and returns its id without waiting for the answer.
    async fn send(&mut self, method: &str, params: Value) -> String {
        let id = Uuid::new_v4().to_string();
        let request = json!({"v":1,"type":"request","id":id,"method":method,"params":params});
        send_json(
            &mut self.ws,
            &self
                .session
                .seal("c2d", &serde_json::to_vec(&request).unwrap())
                .unwrap(),
        )
        .await
        .unwrap();
        id
    }
    /// The next frame from the connector, whatever it answers.
    async fn next(&mut self) -> Reply {
        let frame = timeout(Duration::from_secs(15), receive_json(&mut self.ws))
            .await
            .expect("the connector answers")
            .unwrap();
        let envelope: Envelope = serde_json::from_value(frame).unwrap();
        let plaintext = self.session.open("d2c", &envelope).unwrap();
        let json = link::decode_frame(&plaintext).unwrap();
        let value: Value = serde_json::from_slice(&json).unwrap();
        Reply {
            first_byte: plaintext[0],
            sealed: plaintext.len(),
            json_len: json.len(),
            value,
        }
    }
    async fn compression(&mut self, mode: &str) -> Reply {
        self.call("link.configure", json!({"compression":mode}))
            .await
    }
}
struct Reply {
    first_byte: u8,
    sealed: usize,
    json_len: usize,
    value: Value,
}
impl Reply {
    fn deflated(&self) -> bool {
        self.first_byte == link::FRAME_DEFLATE
    }
    fn code(&self) -> &str {
        self.value["error"]["code"].as_str().unwrap_or("")
    }
}

#[tokio::test]
async fn ready_announces_chats_when_the_cli_has_them_and_only_then() {
    let rig = Rig::new().await;
    let phone = Phone::connect(&rig.pairing).await;
    // Additive: everything an older phone reads is still there.
    assert_eq!(phone.ready["desktop_id"], rig.pairing.desktop_id);
    assert_eq!(phone.ready["features"]["chat"], true, "{}", phone.ready);
    assert_eq!(
        phone.ready["features"]["deflate"]["min_bytes"],
        link::MIN_COMPRESS_BYTES
    );
    assert_eq!(phone.ready["features"]["history_max_lines"], 5000);
    // Files from the phone need nothing of the CLI to be announced.
    assert_eq!(
        phone.ready["features"]["upload"],
        riwork_remote::upload::features()
    );
    // A chat can go on with the other provider, and the phone can ask for its models.
    assert_eq!(phone.ready["features"]["chat_provider_switch"], true);
    assert_eq!(phone.ready["features"]["chat_models"], true);
    assert_eq!(rig.calls("capabilities", "--json"), 1);
    // Believed once it said yes: a second phone does not ask again.
    drop(phone);
    let again = Phone::connect(&rig.pairing).await;
    assert_eq!(again.ready["features"]["chat"], true);
    assert_eq!(again.ready["features"]["chat_provider_switch"], true);
    assert_eq!(rig.calls("capabilities", "--json"), 1);

    // A CLI with chats from before the switch: chats, and neither of the two.
    let rig = Rig::with(|dir| {
        std::fs::write(dir.join("capabilities.out"), "{\"v\":1,\"chat\":true}").unwrap()
    })
    .await;
    let phone = Phone::connect(&rig.pairing).await;
    let features = phone.ready["features"].as_object().unwrap();
    assert_eq!(features["chat"], true);
    assert!(
        !features.contains_key("chat_provider_switch"),
        "{features:?}"
    );
    assert!(!features.contains_key("chat_models"), "{features:?}");

    // A CLI that says no, one that does not know the question, and one that answers it
    // oddly: no `chat` in features at all, and the rest as it was.
    for answer in [
        Some("{\"v\":1,\"chat\":false}"),
        Some("{\"v\":1}"),
        Some("{\"v\":1,\"chat\":\"true\"}"),
        Some("chat yes"),
        None,
    ] {
        let rig = Rig::with(|dir| match answer {
            Some(text) => std::fs::write(dir.join("capabilities.out"), text).unwrap(),
            None => std::fs::write(dir.join("capabilities.unknown"), "").unwrap(),
        })
        .await;
        let mut phone = Phone::connect(&rig.pairing).await;
        let features = phone.ready["features"].as_object().unwrap();
        assert!(!features.contains_key("chat"), "{answer:?}: {features:?}");
        assert!(
            !features.contains_key("chat_provider_switch"),
            "{answer:?}: {features:?}"
        );
        assert!(features.contains_key("deflate"), "{answer:?}");
        // And the methods say why.
        let refused = phone.call("chats.list", json!({})).await;
        assert_eq!(refused.code(), "cli_error", "{answer:?}: {}", refused.value);
        assert!(
            refused.value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("update RiWork")
        );
    }
}

#[tokio::test]
async fn ready_announces_orchestrator_creation_when_the_cli_can_and_only_then() {
    let rig = Rig::new().await;
    let phone = Phone::connect(&rig.pairing).await;
    assert_eq!(
        phone.ready["features"]["orchestrator_create"], true,
        "{}",
        phone.ready
    );
    // The one question about chats and orchestrators is asked once, and believed.
    assert_eq!(phone.ready["features"]["chat"], true);
    assert_eq!(rig.calls("capabilities", "--json"), 1);
    drop(phone);
    let again = Phone::connect(&rig.pairing).await;
    assert_eq!(again.ready["features"]["orchestrator_create"], true);
    assert_eq!(rig.calls("capabilities", "--json"), 1);

    // A CLI with chats but no orchestrator creation, one that says no, one that answers
    // oddly, and one from before the question: no `orchestrator_create` in features, the
    // rest as it was, and the method says why.
    for answer in [
        Some("{\"v\":1,\"chat\":true}"),
        Some("{\"v\":1,\"chat\":true,\"orchestrator_create\":false}"),
        Some("{\"v\":1,\"orchestrator_create\":\"true\"}"),
        None,
    ] {
        let rig = Rig::with(|dir| match answer {
            Some(text) => std::fs::write(dir.join("capabilities.out"), text).unwrap(),
            None => std::fs::write(dir.join("capabilities.unknown"), "").unwrap(),
        })
        .await;
        let mut phone = Phone::connect(&rig.pairing).await;
        let features = phone.ready["features"].as_object().unwrap();
        assert!(
            !features.contains_key("orchestrator_create"),
            "{answer:?}: {features:?}"
        );
        assert!(features.contains_key("deflate"), "{answer:?}");
        assert_eq!(
            features.contains_key("chat"),
            answer.is_some_and(|text| text.contains("\"chat\":true")),
            "{answer:?}"
        );
        let refused = phone.call("orchestrator.create", json!({})).await;
        assert_eq!(refused.code(), "cli_error", "{answer:?}: {}", refused.value);
        assert!(
            refused.value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("update RiWork")
        );
        assert_eq!(rig.calls("orchestrator", "create"), 0, "{answer:?}");
    }
}

#[tokio::test]
async fn ready_announces_agents_that_follow_the_desktop_settings_when_the_cli_can() {
    let rig = Rig::new().await;
    let phone = Phone::connect(&rig.pairing).await;
    assert_eq!(
        phone.ready["features"]["shell_create_as_settings"], true,
        "{}",
        phone.ready
    );
    // One question answers it with chats and orchestrators, and a yes is believed.
    assert_eq!(rig.calls("capabilities", "--json"), 1);
    drop(phone);
    let again = Phone::connect(&rig.pairing).await;
    assert_eq!(again.ready["features"]["shell_create_as_settings"], true);
    assert_eq!(rig.calls("capabilities", "--json"), 1);

    // A CLI without it, one that says no, one that answers oddly and one from before the
    // question: not announced, the rest as it was, and still one question per handshake.
    for answer in [
        Some("{\"v\":1,\"chat\":true,\"orchestrator_create\":true}"),
        Some("{\"v\":1,\"chat\":true,\"shell_create_as_settings\":false}"),
        Some("{\"v\":1,\"shell_create_as_settings\":\"true\"}"),
        None,
    ] {
        let rig = Rig::with(|dir| match answer {
            Some(text) => std::fs::write(dir.join("capabilities.out"), text).unwrap(),
            None => std::fs::write(dir.join("capabilities.unknown"), "").unwrap(),
        })
        .await;
        let phone = Phone::connect(&rig.pairing).await;
        let features = phone.ready["features"].as_object().unwrap();
        assert!(
            !features.contains_key("shell_create_as_settings"),
            "{answer:?}: {features:?}"
        );
        assert!(features.contains_key("deflate"), "{answer:?}");
        assert_eq!(rig.calls("capabilities", "--json"), 1, "{answer:?}");
    }
}

#[tokio::test]
async fn an_orchestrator_being_made_is_finished_when_the_phone_goes_away() {
    let rig = Rig::new().await;
    let entry = json!({
        "id": rig.chat, "project_id": null, "worktree_id": null, "kind": "orchestrator",
        "cwd": "/Users/me/orchestrator", "harness": "codex", "alive": true,
        "created_at_unix": 1790000000u64, "mode": "chat", "chat_id": rig.chat,
        "provider": "codex", "created": true
    });
    rig.canned("orchestrator.json", &entry);
    std::fs::write(rig.dir.path().join("orchestrator.delay"), "1").unwrap();
    let mut phone = Phone::connect(&rig.pairing).await;
    phone.send("orchestrator.create", json!({})).await;
    // Wait for the CLI to be running, then the phone leaves.
    for _ in 0..200 {
        if rig.calls("orchestrator", "create") > 0 {
            break;
        }
        sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(rig.calls("orchestrator", "create"), 1);
    drop(phone);
    for _ in 0..200 {
        if rig.dir.path().join("orchestrator.ran").exists() {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(
        rig.dir.path().join("orchestrator.ran").exists(),
        "the CLI was cut short"
    );
    // A phone that comes back asks again and is handed the orchestrator that now exists,
    // which the CLI says it did not make this time.
    std::fs::write(
        rig.dir.path().join("orchestrator.json"),
        json!({
            "id": rig.chat, "project_id": null, "worktree_id": null, "kind": "orchestrator",
            "cwd": "/Users/me/orchestrator", "harness": "codex", "alive": true,
            "created_at_unix": 1790000000u64, "mode": "chat", "chat_id": rig.chat,
            "provider": "codex", "created": false
        })
        .to_string(),
    )
    .unwrap();
    let mut phone = Phone::connect(&rig.pairing).await;
    let again = phone.call("orchestrator.create", json!({})).await;
    assert_eq!(again.value["ok"], true, "{}", again.value);
    assert_eq!(again.value["result"]["created"], false);
    assert_eq!(again.value["result"]["orchestrator"]["id"], rig.chat);
    assert_eq!(again.value["result"]["orchestrator"]["chat_id"], rig.chat);
}

#[tokio::test]
#[ignore = "slow: real relay and connector; asserts wall-clock windows of 1.8 s, 1.9 s and 3.9 s"]
async fn waiting_for_events_leaves_room_for_reads_and_for_typing_and_a_third_wait_queues() {
    let rig = Rig::new().await;
    rig.events(1, |_| "hi".into());
    std::fs::write(rig.dir.path().join("events.delay"), "2").unwrap();
    let mut phone = Phone::connect(&rig.pairing).await;
    let wait = json!({"chat_id":rig.chat,"since":0,"wait_ms":5000});
    let started = Instant::now();
    let polls = [
        phone.send("chat.events", wait.clone()).await,
        phone.send("chat.events", wait.clone()).await,
        phone.send("chat.events", wait.clone()).await,
    ];
    // A read, a message to the chat and a stop are answered while two polls are parked.
    let list = phone.send("chats.list", json!({})).await;
    let command = phone
        .send(
            "chat.command",
            json!({"chat_id":rig.chat,"command":{"command":"send","text":"hello"}}),
        )
        .await;
    let stop = phone.send("chat.stop", json!({"chat_id":rig.chat})).await;
    let mut replies = Vec::new();
    for _ in 0..6 {
        let reply = phone.next().await;
        replies.push((
            reply.value["id"].as_str().unwrap().to_owned(),
            started.elapsed(),
            reply,
        ));
    }
    let position = |id: &String| replies.iter().position(|(got, ..)| got == id).unwrap();
    for quick in [&list, &command, &stop] {
        let (_, took, reply) = &replies[position(quick)];
        assert_eq!(reply.value["ok"], true, "{}", reply.value);
        assert!(
            *took < Duration::from_millis(1800),
            "answered while the polls wait: {took:?}"
        );
    }
    assert_eq!(
        replies[position(&command)].2.value["result"],
        json!({"status":"ok"})
    );
    assert_eq!(
        replies[position(&stop)].2.value["result"],
        json!({"status":"stopped"})
    );
    // The first two polls end together, after the wait; the third could only start then.
    for poll in &polls[..2] {
        assert!(replies[position(poll)].1 >= Duration::from_millis(1900));
    }
    let third = &replies[position(&polls[2])];
    assert!(third.1 >= Duration::from_millis(3900), "{:?}", third.1);
    for poll in &polls {
        let (_, _, reply) = &replies[position(poll)];
        assert_eq!(reply.value["result"]["events"].as_array().unwrap().len(), 1);
        assert_eq!(reply.value["result"]["next"], 1);
        assert!(reply.value["server_ms"].as_u64().unwrap() >= 1900);
    }
    assert_eq!(rig.calls("chat", "events"), 3);
}

#[tokio::test]
async fn a_page_is_sealed_as_the_session_asked_and_always_fits_one_frame() {
    let rig = Rig::new().await;
    // 300 events of about 900 bytes of one sentence: 300 KB of JSON that deflates to a few KB.
    rig.events(300, |_| "the same sentence again. ".repeat(36));
    let mut phone = Phone::connect(&rig.pairing).await;
    let params = json!({"chat_id":rig.chat,"since":0,"wait_ms":0});
    // A phone that never opted in cannot be sent more than a frame of plain JSON, and the
    // CLI is told so: the page it is asked for is a margin short of a frame. (This stand-in
    // prints its whole page, which is over the cap: the connector refuses it.)
    let plain = phone.call("chat.events", params.clone()).await;
    assert!(!plain.deflated());
    assert_eq!(plain.code(), "response_too_large", "{}", plain.value);
    let asked = std::fs::read_to_string(rig.dir.path().join("calls.log")).unwrap();
    let budget = (MAX_PLAINTEXT - 1024).to_string();
    assert!(
        asked
            .lines()
            .any(|l| l.starts_with("chat events") && l.contains(&format!("--max-bytes {budget}"))),
        "{asked}"
    );

    // Once it opted in, the same page comes whole: far more JSON than a frame, deflated into one.
    phone.compression("deflate").await;
    let whole = phone.call("chat.events", params.clone()).await;
    assert_eq!(whole.value["ok"], true, "{}", whole.value);
    assert!(whole.deflated());
    assert!(whole.json_len > MAX_PLAINTEXT && whole.sealed <= MAX_PLAINTEXT);
    assert_eq!(
        whole.value["result"]["events"].as_array().unwrap().len(),
        300
    );
    assert_eq!(whole.value["result"]["more"], false);
    let asked = std::fs::read_to_string(rig.dir.path().join("calls.log")).unwrap();
    let budget = (link::MAX_INFLATED - 1024).to_string();
    assert!(
        asked
            .lines()
            .any(|l| l.contains(&format!("--max-bytes {budget}"))),
        "{asked}"
    );

    // Noise does not shrink: the page is cut to the events that fit, says there are more,
    // and the cut is where the next call goes on.
    rig.events(150, |seq| noise(4000, seq));
    let cut = phone.call("chat.events", params).await;
    assert_eq!(cut.value["ok"], true, "{}", cut.value);
    assert!(cut.sealed <= MAX_PLAINTEXT);
    let held = cut.value["result"]["events"].as_array().unwrap();
    assert!(!held.is_empty() && held.len() < 150, "{}", held.len());
    assert_eq!(cut.value["result"]["more"], true);
    assert_eq!(cut.value["result"]["next"], held.last().unwrap()["seq"]);
    assert_eq!(held[0]["seq"], 1);
}
