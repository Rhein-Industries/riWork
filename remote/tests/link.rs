//! The link extension end to end: a real relay and the real connector binary, a
//! stand-in CLI that prints canned pages, and a phone that speaks the frames.
//! `server_ms` on every reply; compression announced in `ready`, opted into with
//! `link.configure`, applied per reply; the larger limits that go with it.
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
    shell: String,
    connector: Child,
    relay: tokio::task::JoinHandle<()>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.connector.start_kill();
        self.relay.abort();
    }
}

/// A CLI that lists one live shell and prints `history.json` / `output.json` from its
/// directory for `shell history` / `shell output`. Every `shell history` call is logged in `history.log`;
/// with the file `cli-limit` (a number) it refuses longer pages the way an older build does, and with
/// the file `slow` it takes a second.
fn stand_in_cli(dir: &Path, shell: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let cli = dir.join("canned-riwork");
    let script = r#"#!/bin/sh
D='@DIR@'
case "$1 $2" in
'shell list') printf '[{"id":"@SHELL@","alive":true}]';;
'orchestrator list'|'project list') echo '[]';;
'shell history')
  echo "$*" >> "$D/history.log"
  M=''; P=''; for A in "$@"; do if [ "$P" = '--lines' ]; then M="$A"; fi; P="$A"; done
  if [ -e "$D/cli-limit" ] && [ "$M" -gt "$(cat "$D/cli-limit")" ]; then
    echo "riwork: --lines needs an integer from 1 to $(cat "$D/cli-limit")" >&2; exit 2
  fi
  if [ -e "$D/slow" ]; then sleep 1; fi
  cat "$D/history.json";;
'shell output') cat "$D/output.json";;
esac
"#
    .replace("@DIR@", &dir.to_string_lossy())
    .replace("@SHELL@", shell);
    std::fs::write(&cli, script).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}

impl Rig {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
        let storage = Storage::at(home.clone()).unwrap();
        let routes_path = dir.path().join("routes.json");
        let pairing = storage
            .pair(
                url,
                "link test".into(),
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
        let shell = Uuid::new_v4().to_string();
        let cli = stand_in_cli(dir.path(), &shell);
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
            shell,
            connector,
            relay,
        }
    }
    fn canned(&self, name: &str, value: &Value) {
        std::fs::write(self.dir.path().join(name), value.to_string()).unwrap();
    }
    /// A page of `lines` styled lines, as the CLI prints it.
    fn history_page(&self, lines: usize, history_size: usize) {
        let text = styled_text(lines);
        self.canned(
            "history.json",
            &json!({"output":text,"line_count":lines,"history_size":history_size,"complete":false}),
        );
    }
}

fn styled_text(lines: usize) -> String {
    (0..lines)
        .map(|i| {
            format!(
                "\u{1b}[38;5;{}mcargo test --lib case_{i} ... \u{1b}[32mok\u{1b}[0m",
                30 + i % 30
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
/// Text no deflate can shrink: a xorshift stream over the printable ASCII range.
fn noise(bytes: usize) -> String {
    let mut x = 0x2545_F491_4F6C_DD1Du64;
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
        let wire = frame.to_string().len();
        let envelope: Envelope = serde_json::from_value(frame).unwrap();
        let plaintext = self.session.open("d2c", &envelope).unwrap();
        let json = link::decode_frame(&plaintext).unwrap();
        let value: Value = serde_json::from_slice(&json).unwrap();
        Reply {
            first_byte: plaintext[0],
            sealed: plaintext.len(),
            json_len: json.len(),
            wire,
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
    wire: usize,
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
async fn ready_announces_what_the_desktop_can_do_and_every_reply_reports_server_ms() {
    let rig = Rig::new().await;
    let mut phone = Phone::connect(&rig.pairing).await;
    // Additive: the three fields an older phone reads are still there.
    assert_eq!(phone.ready["desktop_id"], rig.pairing.desktop_id);
    assert_eq!(phone.ready["device_id"], rig.pairing.device_id);
    let features = &phone.ready["features"];
    assert_eq!(features["deflate"]["min_bytes"], link::MIN_COMPRESS_BYTES);
    assert_eq!(features["deflate"]["max_inflated"], link::MAX_INFLATED);
    assert_eq!(features["history_max_lines"], link::HISTORY_MAX_LINES);

    let ok = phone.call("projects.list", json!({})).await;
    assert_eq!(ok.value["ok"], true, "{}", ok.value);
    assert_eq!(ok.value["result"]["projects"], json!([]));
    assert!(ok.value["server_ms"].is_u64(), "{}", ok.value);
    assert!(!ok.deflated());
    // An error carries it too, and so does one for a request that is refused outright.
    let bad = phone.call("no.such.method", json!({})).await;
    assert_eq!(bad.code(), "invalid_request");
    assert!(bad.value["server_ms"].is_u64(), "{}", bad.value);
    assert_eq!(bad.value["id"].as_str().map(str::len), Some(36));
}

#[tokio::test]
async fn after_the_opt_in_large_replies_are_deflated_and_small_ones_are_not() {
    let rig = Rig::new().await;
    rig.history_page(4000, 9000);
    let mut phone = Phone::connect(&rig.pairing).await;

    let answer = phone.compression("deflate").await;
    assert_eq!(answer.value["ok"], true, "{}", answer.value);
    assert_eq!(answer.value["result"]["compression"], "deflate");
    assert!(!answer.deflated(), "the answer itself is small");

    let small = phone.call("projects.list", json!({})).await;
    assert!(!small.deflated(), "under the minimum a reply is left alone");

    // 4000 lines of styled text is past one frame's 128 KiB as JSON, and fits as deflate.
    let page = phone
        .call(
            "shell.history",
            json!({"shell_id":rig.shell,"end":0,"lines":4000,"styled":true}),
        )
        .await;
    assert!(page.deflated(), "{}", page.value);
    assert_eq!(page.value["ok"], true);
    assert_eq!(page.value["result"]["line_count"], 4000);
    assert!(page.value["server_ms"].is_u64());
    assert!(
        page.json_len > MAX_PLAINTEXT,
        "{} bytes of JSON",
        page.json_len
    );
    assert!(page.sealed <= MAX_PLAINTEXT);
    assert!(
        page.sealed * 4 < page.json_len,
        "styled text shrinks a lot: {} -> {}",
        page.json_len,
        page.sealed
    );
    // Byte for byte what the CLI printed.
    let text = styled_text(4000);
    assert_eq!(page.value["result"]["output"], text.as_str());
    assert!(page.wire < page.json_len / 3);

    // Back off: the old limit applies again.
    let off = phone.compression("none").await;
    assert_eq!(off.value["result"]["compression"], "none");
    let again = phone
        .call(
            "shell.history",
            json!({"shell_id":rig.shell,"end":0,"lines":4000,"styled":true}),
        )
        .await;
    assert_eq!(again.code(), "response_too_large");
    assert!(!again.deflated());
}

#[tokio::test]
async fn a_reply_that_does_not_fit_a_frame_even_deflated_is_response_too_large() {
    let rig = Rig::new().await;
    // 300 KB of noise: well under the inflate limit, far over a frame once deflated.
    rig.canned(
        "history.json",
        &json!({"output":noise(300_000),"line_count":1,"history_size":9000,"complete":false}),
    );
    let mut phone = Phone::connect(&rig.pairing).await;
    phone.compression("deflate").await;
    let reply = phone
        .call(
            "shell.history",
            json!({"shell_id":rig.shell,"end":0,"lines":1}),
        )
        .await;
    assert_eq!(reply.code(), "response_too_large", "{}", reply.value);
    assert!(reply.value["server_ms"].is_u64());
    // And the session carries on.
    let ok = phone.call("projects.list", json!({})).await;
    assert_eq!(ok.value["ok"], true);
}

#[tokio::test]
async fn link_configure_refuses_bad_params_and_a_new_connection_starts_plain() {
    let rig = Rig::new().await;
    rig.history_page(4000, 9000);
    let mut phone = Phone::connect(&rig.pairing).await;
    for params in [
        json!({"compression":"zstd"}),
        json!({"compression":"deflate","extra":1}),
        json!({"compression":true}),
        json!([]),
    ] {
        let reply = phone.call("link.configure", params).await;
        assert_eq!(reply.code(), "invalid_request", "{}", reply.value);
    }
    // Refused, so nothing changed: still plain.
    let page = json!({"shell_id":rig.shell,"end":0,"lines":4000,"styled":true});
    assert_eq!(
        phone.call("shell.history", page.clone()).await.code(),
        "response_too_large"
    );
    phone.compression("deflate").await;
    assert!(phone.call("shell.history", page.clone()).await.deflated());
    drop(phone);

    // Compression belongs to the session: the next one starts without it.
    let mut again = Phone::connect(&rig.pairing).await;
    assert_eq!(
        again.call("shell.history", page.clone()).await.code(),
        "response_too_large"
    );
    again.compression("deflate").await;
    assert!(again.call("shell.history", page).await.deflated());
}

#[tokio::test]
async fn a_cli_that_takes_fewer_lines_than_announced_is_learned_once_and_announced_after() {
    let rig = Rig::new().await;
    rig.history_page(900, 9000);
    std::fs::write(rig.dir.path().join("cli-limit"), "1000").unwrap();
    let calls = |rig: &Rig| {
        std::fs::read_to_string(rig.dir.path().join("history.log"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    let mut phone = Phone::connect(&rig.pairing).await;
    assert_eq!(phone.ready["features"]["history_max_lines"], 5000);
    let page = |lines: u32| json!({"shell_id":rig.shell,"end":0,"lines":lines,"styled":true});
    // The first page that is too long goes to the CLI, which says what it takes.
    let refused = phone.call("shell.history", page(2000)).await;
    assert_eq!(refused.code(), "cli_error", "{}", refused.value);
    assert!(
        refused.value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("from 1 to 1000"),
        "{}",
        refused.value
    );
    assert_eq!(calls(&rig), 1);
    // After that the connector refuses by itself, without running anything.
    let again = phone.call("shell.history", page(2000)).await;
    assert_eq!(again.code(), "invalid_request", "{}", again.value);
    assert!(
        again.value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("1..=1000")
    );
    assert_eq!(calls(&rig), 1, "the CLI was not asked again");
    let fits = phone.call("shell.history", page(1000)).await;
    assert_eq!(fits.value["ok"], true, "{}", fits.value);
    drop(phone);
    // And the next session is told.
    let next = Phone::connect(&rig.pairing).await;
    assert_eq!(next.ready["features"]["history_max_lines"], 1000);
}

#[tokio::test]
async fn link_configure_is_answered_at_once_and_a_page_still_being_captured_follows_it_deflated() {
    let rig = Rig::new().await;
    rig.history_page(300, 9000);
    std::fs::write(rig.dir.path().join("slow"), "").unwrap();
    let mut phone = Phone::connect(&rig.pairing).await;
    let page = phone
        .send(
            "shell.history",
            json!({"shell_id":rig.shell,"end":0,"lines":300,"styled":true}),
        )
        .await;
    let configure = phone
        .send("link.configure", json!({"compression":"deflate"}))
        .await;
    let first = phone.next().await;
    assert_eq!(first.value["id"], configure, "{}", first.value);
    assert_eq!(first.value["result"]["compression"], "deflate");
    assert!(!first.deflated());
    // The form of a reply is chosen when it is sealed: this one was started before the opt-in.
    let second = phone.next().await;
    assert_eq!(second.value["id"], page, "{}", second.value);
    assert_eq!(second.value["ok"], true);
    assert!(second.deflated(), "{} bytes of JSON", second.json_len);
    assert!(
        second.value["server_ms"].as_u64().unwrap() >= 900,
        "{}",
        second.value
    );
    // Asking without a setting changes nothing and says what is.
    let said = phone.call("link.configure", json!({})).await;
    assert_eq!(said.value["result"]["compression"], "deflate");
}

#[test]
fn the_published_frames_decode_and_the_bad_ones_are_refused() {
    // Made by Python's zlib and by this crate (fixtures/generate_link.py); the iOS tests read the same file.
    let fixture: Value = serde_json::from_str(include_str!("../fixtures/link.json")).unwrap();
    assert_eq!(fixture["max_inflated"], link::MAX_INFLATED);
    for vector in fixture["vectors"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap();
        let frame = hex::decode(vector["frame_hex"].as_str().unwrap()).unwrap();
        let json = link::decode_frame(&frame).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert_eq!(
            String::from_utf8(json.clone()).unwrap(),
            vector["json"],
            "{name}"
        );
        let value: Value = serde_json::from_slice(&json).unwrap();
        assert!(value["server_ms"].is_u64(), "{name}");
    }
    for vector in fixture["refused"].as_array().unwrap() {
        let frame = hex::decode(vector["frame_hex"].as_str().unwrap()).unwrap();
        assert!(
            link::decode_frame(&frame).is_err(),
            "{}",
            vector["name"].as_str().unwrap()
        );
    }
}
