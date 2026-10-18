//! `dashboard.get` (docs/remote-protocol.md, "Dashboard extension"): validation before any CLI
//! runs, the one read it turns into (`riwork dashboard --json`), what it passes on of the CLI's
//! reply (the contract's keys, each list cut to its limit, objects only, version 1 only), the
//! capability that gates it and `features.dashboard` in `ready`. Against a stub CLI that records
//! its argv and prints whatever `dashboard.json` holds, and against the shared fixture the
//! engine writes and the phone decodes (`ios/Tests/Fixtures/dashboard.json`).
use riwork_remote::{
    MAX_PLAINTEXT,
    config::{Pairing, Storage, private_read},
    connector::{connect_registered, receive_json, send_json},
    crypto::{Envelope, ServerHello, Session, accept_server, client_hello, decode, random32},
    lanes::{self, Lane},
    link,
    relay::{Relay, Routes},
    rpc::{DASHBOARD_AUTOMATIONS_MAX, DASHBOARD_KEYS, DASHBOARD_LISTS, Rpc},
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

const SHARED_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ios/Tests/Fixtures/dashboard.json"
);

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn req(method: &str, params: Value) -> Value {
    json!({"v":1,"type":"request","id":new_uuid(),"method":method,"params":params})
}
fn code(response: &Value) -> &str {
    response["error"]["code"].as_str().unwrap_or("")
}
fn message(response: &Value) -> &str {
    response["error"]["message"].as_str().unwrap_or("")
}
fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}
/// Every case of the shared fixture, by name.
fn shared_cases() -> serde_json::Map<String, Value> {
    let text = std::fs::read_to_string(SHARED_FIXTURE).unwrap();
    serde_json::from_str::<Value>(&text)
        .unwrap()
        .as_object()
        .unwrap()
        .clone()
}
fn busy() -> Value {
    shared_cases()["busy"].clone()
}

struct Fixture {
    _storage_dir: tempfile::TempDir,
    stub: tempfile::TempDir,
    rpc: Rpc,
    device: String,
}
impl Fixture {
    fn new() -> Self {
        let storage_dir = tempfile::tempdir().unwrap();
        let stub = tempfile::tempdir().unwrap();
        let storage = Storage::at(storage_dir.path().into()).unwrap();
        let device = storage
            .pair(
                "wss://example.com/v1/ws".into(),
                "phone".into(),
                false,
                &storage_dir.path().join("phone.json"),
                None,
            )
            .unwrap()
            .device_id;
        Self {
            rpc: Rpc::new(stub_cli(stub.path()), storage),
            _storage_dir: storage_dir,
            stub,
            device,
        }
    }
    /// Every request must be answered before a CLI would run.
    fn without_cli() -> Self {
        let mut fixture = Self::new();
        fixture.rpc = Rpc::new(
            "/nonexistent/no-CLI-may-be-executed".into(),
            fixture.rpc.storage.clone(),
        );
        fixture
    }
    async fn get(&self, params: Value) -> Value {
        self.rpc
            .handle(&self.device, req("dashboard.get", params))
            .await
            .unwrap()
    }
    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.stub.path().join(name), value).unwrap();
    }
    fn cli_says(&self, value: &Value) {
        self.set("dashboard.json", &value.to_string());
    }
    fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.stub.path().join("argv.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                line.split('\u{1f}')
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .collect()
    }
}

/// Logs each call (arguments separated by U+001F). `capabilities` prints `capabilities.out`
/// (default: this CLI has the dashboard), or refuses the command if `capabilities.unknown`
/// exists. `dashboard` prints `dashboard.json`, or fails with the line in `dashboard.error`.
/// Anything else it is asked fails, so a call the connector must not make cannot pass.
fn stub_cli(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let cli = dir.join("fake-riwork");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\n\
             d='{dir}'\n\
             for a in \"$@\"; do printf '%s\\037' \"$a\"; done >> \"$d/argv.log\"\n\
             printf '\\n' >> \"$d/argv.log\"\n\
             case \"$1 $2\" in\n\
             'capabilities --json')\n\
               if [ -e \"$d/capabilities.unknown\" ]; then echo \"riwork: Unknown invocation 'capabilities'\" >&2; exit 2; fi\n\
               if [ -e \"$d/capabilities.out\" ]; then cat \"$d/capabilities.out\"; else printf '{{\"v\":1,\"chat\":true,\"dashboard\":true}}'; fi;;\n\
             'dashboard --json')\n\
               if [ -e \"$d/dashboard.error\" ]; then printf 'riwork: %s\\n' \"$(cat \"$d/dashboard.error\")\" >&2; exit 2; fi\n\
               cat \"$d/dashboard.json\";;\n\
             *) echo \"riwork: not expected: $*\" >&2; exit 9;;\n\
             esac\n",
            dir = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}

#[tokio::test]
async fn parameters_are_exactly_an_empty_object_checked_before_any_cli_runs() {
    let f = Fixture::without_cli();
    let bad = [
        json!(null),
        json!([]),
        json!("dashboard"),
        json!(7),
        json!(true),
        json!({"project_id": new_uuid()}),
        json!({"limit": 10}),
        json!({"json": true}),
        json!({"v": 1}),
        json!({"compression": "deflate"}),
        json!({"": null}),
    ];
    for params in bad {
        let response = f.get(params.clone()).await;
        assert_eq!(response["ok"], false, "{params}: {response}");
        assert_eq!(code(&response), "invalid_request", "{params}: {response}");
    }
    // `{}` is accepted, and only then is the CLI asked anything: here it cannot be run at all.
    let response = f.get(json!({})).await;
    assert_eq!(code(&response), "cli_error", "{response}");

    let f = Fixture::new();
    f.cli_says(&busy());
    for params in [json!({"all": true}), json!(null), json!([])] {
        let response = f.get(params.clone()).await;
        assert_eq!(code(&response), "invalid_request", "{params}: {response}");
    }
    assert!(f.calls().is_empty(), "{:?}", f.calls());
}

#[tokio::test]
async fn without_the_capability_the_method_is_unsupported_and_nothing_else_is_asked() {
    for answer in [
        Some("{\"v\":1,\"chat\":true,\"orchestrator_create\":true}"),
        Some("{\"v\":1,\"dashboard\":false}"),
        Some("{\"v\":1,\"dashboard\":\"true\"}"),
        Some("{\"v\":2,\"dashboard\":true}"),
        Some("dashboard yes"),
        None,
    ] {
        let f = Fixture::new();
        match answer {
            Some(text) => f.set("capabilities.out", text),
            None => f.set("capabilities.unknown", ""),
        }
        f.cli_says(&busy());
        let response = f.get(json!({})).await;
        assert_eq!(code(&response), "invalid_request", "{answer:?}: {response}");
        // The words a connector from before the method answers with, which the phone detects.
        assert_eq!(message(&response), "unsupported RPC method", "{answer:?}");
        assert_eq!(f.calls(), [argv(&["capabilities", "--json"])], "{answer:?}");
    }
}

#[tokio::test]
async fn with_the_capability_it_is_one_read_and_the_engines_reply_passes_unchanged() {
    let f = Fixture::new();
    let cases = shared_cases();
    for (name, case) in &cases {
        f.cli_says(case);
        let response = f.get(json!({})).await;
        assert_eq!(response["ok"], true, "{name}: {response}");
        assert_eq!(&response["result"], case, "{name}");
    }
    // The question once, believed, and then exactly one `dashboard --json` per request: it
    // starts, creates and changes nothing, and asks the CLI nothing else.
    let mut expected = vec![argv(&["capabilities", "--json"])];
    expected.extend((0..cases.len()).map(|_| argv(&["dashboard", "--json"])));
    assert_eq!(f.calls(), expected);
}

#[tokio::test]
async fn the_reply_keeps_only_the_contract_and_each_list_within_its_limit() {
    let f = Fixture::new();
    let mut cli = busy();
    let row = cli["attention"][0].clone();
    let mut attention: Vec<Value> = (0..45).map(|_| row.clone()).collect();
    attention.insert(1, json!("not an object"));
    attention.insert(3, json!(null));
    attention.insert(5, json!([1, 2]));
    cli["attention"] = json!(attention);
    cli["attention_total"] = json!(45);
    let project = cli["projects"][0].clone();
    cli["projects"] = json!((0..160).map(|_| project.clone()).collect::<Vec<_>>());
    cli["projects_total"] = json!(160);
    let item = cli["automations"]["items"][0].clone();
    cli["automations"]["items"] = json!((0..35).map(|_| item.clone()).collect::<Vec<_>>());
    cli["automations"]["items_total"] = json!(35);
    cli["finished"] = json!([7, "x", false]);
    let provider = cli["backends"]["providers"][0].clone();
    let mut providers = vec![provider.clone(); 12];
    providers.insert(1, json!("not an object"));
    providers.insert(3, json!(null));
    cli["backends"]["providers"] = json!(providers);
    cli["backends"]["providers_total"] = json!(12);
    cli["root"] = json!("/Users/me/secret");
    cli["model"] = json!("gpt-secret");
    cli["debug"] = json!({"argv": ["riwork"]});
    cli["decisions"] = json!((0..45).map(|n| json!({"id":n})).collect::<Vec<_>>());
    cli["decisions_total"] = json!(45);
    cli["decision_sources"] =
        json!({"state":"partial","sampled_at":7,"chats_read":1,"chats_complete":0,"chats_total":1});
    cli["recent_activity"] = json!(
        (0..42)
            .map(|n| json!({"source":{"event_seq":n}}))
            .collect::<Vec<_>>()
    );
    cli["recent_activity_total"] = json!(42);
    cli["intelligence"] = json!({"state":"ready","summary":"Observed work","highlights":[],"followups":[],"stale":false});
    f.cli_says(&cli);

    let response = f.get(json!({})).await;
    assert_eq!(response["ok"], true, "{response}");
    let result = response["result"].as_object().unwrap();
    let mut keys: Vec<&str> = result.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut contract = DASHBOARD_KEYS.to_vec();
    contract.sort_unstable();
    assert_eq!(keys, contract);
    // Non-objects are dropped first, then the tail is cut; the totals are the CLI's.
    let attention = result["attention"].as_array().unwrap();
    assert_eq!(attention.len(), 40);
    assert!(attention.iter().all(|row| *row == busy()["attention"][0]));
    assert_eq!(result["attention_total"], 45);
    assert_eq!(result["projects"].as_array().unwrap().len(), 150);
    assert_eq!(result["projects_total"], 160);
    assert_eq!(
        result["automations"]["items"].as_array().unwrap().len(),
        DASHBOARD_AUTOMATIONS_MAX
    );
    assert_eq!(result["automations"]["items_total"], 35);
    assert_eq!(result["finished"], json!([]));
    assert_eq!(result["decisions"].as_array().unwrap().len(), 40);
    assert_eq!(result["decisions_total"], 45);
    assert_eq!(result["recent_activity"].as_array().unwrap().len(), 40);
    assert_eq!(result["decision_sources"], cli["decision_sources"]);
    assert_eq!(
        result["backends"]["providers"],
        json!(vec![
            provider;
            riwork_engine::global_overview::WIRE_MAX_PROVIDERS
        ])
    );
    assert_eq!(result["backends"]["providers_total"], 12);
    assert_eq!(result["backends"]["app"], busy()["backends"]["app"]);
    assert_eq!(
        result["backends"]["chat_host"],
        busy()["backends"]["chat_host"]
    );
    // What was within the contract is passed on as it is.
    for key in [
        "totals",
        "global",
        "working",
        "tasks_in_progress",
        "generated_at",
    ] {
        assert_eq!(result[key], busy()[key], "{key}");
    }
}

#[tokio::test]
async fn backend_status_is_additive_for_older_clis_and_unread_samples() {
    let f = Fixture::new();
    let mut legacy = busy();
    legacy.as_object_mut().unwrap().remove("backends");
    f.cli_says(&legacy);
    let response = f.get(json!({})).await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"], legacy);

    legacy["backends"] = json!({"state": "not_read"});
    f.cli_says(&legacy);
    let response = f.get(json!({})).await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"], legacy);
}

#[tokio::test]
async fn a_reply_of_another_version_or_shape_is_a_cli_error() {
    let f = Fixture::new();
    let mut wrong = Vec::new();
    for v in [json!(2), json!(0), json!("1"), json!(1.5), Value::Null] {
        let mut cli = busy();
        cli["v"] = v;
        wrong.push(cli);
    }
    let mut unversioned = busy();
    unversioned.as_object_mut().unwrap().remove("v");
    wrong.push(unversioned);
    wrong.push(json!([busy()]));
    wrong.push(json!("dashboard"));
    for list in ["attention", "projects", "working"] {
        let mut cli = busy();
        cli[list] = json!({"0": {}});
        wrong.push(cli);
    }
    let mut items = busy();
    items["automations"]["items"] = json!("none");
    wrong.push(items);
    let mut providers = busy();
    providers["backends"]["providers"] = json!({"0": {}});
    wrong.push(providers);
    for cli in wrong {
        f.cli_says(&cli);
        let response = f.get(json!({})).await;
        assert_eq!(code(&response), "cli_error", "{cli}: {response}");
        assert!(response.get("result").is_none());
    }
    // Not JSON at all.
    f.set("dashboard.json", "{ half a dashboard");
    let response = f.get(json!({})).await;
    assert_eq!(code(&response), "cli_error", "{response}");
}

#[tokio::test]
async fn a_failing_cli_is_a_cli_error_and_an_oversized_one_says_so() {
    let f = Fixture::new();
    f.set("dashboard.error", "Cannot lock store: Resource busy");
    let response = f.get(json!({})).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    assert!(
        message(&response).contains("Cannot lock store: Resource busy"),
        "{response}"
    );
    std::fs::remove_file(f.stub.path().join("dashboard.error")).unwrap();
    // More than one encrypted reply holds, before anything is trimmed.
    let mut huge = busy();
    huge["padding"] = json!("x".repeat(MAX_PLAINTEXT));
    f.cli_says(&huge);
    let response = f.get(json!({})).await;
    assert_eq!(code(&response), "response_too_large", "{response}");
    assert!(message(&response).contains("link.configure"), "{response}");
}

#[test]
fn it_is_a_read_in_a_shared_slot() {
    let request = req("dashboard.get", json!({}));
    assert_eq!(lanes::classify(&request), Lane::Read);
    assert!(Lane::Read.cancellable());
}

#[test]
fn the_connectors_limits_and_keys_are_the_engines() {
    use riwork_engine::global_overview as engine;
    assert_eq!(
        DASHBOARD_LISTS,
        [
            ("attention", engine::WIRE_MAX_ATTENTION),
            ("working", engine::WIRE_MAX_WORKING),
            ("finished", engine::WIRE_MAX_FINISHED),
            ("projects", engine::WIRE_MAX_PROJECTS),
            ("tasks_in_progress", engine::WIRE_MAX_TASKS_IN_PROGRESS),
            ("backends.providers", engine::WIRE_MAX_PROVIDERS),
            ("decisions", engine::observed::MAX_DECISIONS),
            ("recent_activity", engine::observed::MAX_RECENT),
        ]
    );
    assert_eq!(DASHBOARD_AUTOMATIONS_MAX, engine::WIRE_MAX_AUTOMATIONS);
    for (name, case) in shared_cases() {
        let mut keys: Vec<String> = case.as_object().unwrap().keys().cloned().collect();
        keys.sort_unstable();
        // Shared fixtures deliberately retain the original v1 shape: the new fields are
        // additive and an older CLI is still accepted unchanged.
        let mut contract: Vec<String> = DASHBOARD_KEYS
            .iter()
            .filter(|key| {
                ![
                    "decisions",
                    "decisions_total",
                    "decision_sources",
                    "recent_activity",
                    "recent_activity_total",
                    "intelligence",
                ]
                .contains(key)
            })
            .map(|k| (*k).to_owned())
            .collect();
        contract.sort_unstable();
        assert_eq!(keys, contract, "{name}");
        assert_eq!(case["v"], engine::WIRE_VERSION, "{name}");
    }
}

// ---- `ready` and the method over a real link ---------------------------------------------------

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A relay, the real connector binary and the stub CLI.
struct Rig {
    dir: tempfile::TempDir,
    pairing: Pairing,
    connector: Child,
    relay: tokio::task::JoinHandle<()>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.connector.start_kill();
        self.relay.abort();
    }
}
impl Rig {
    /// `prepare` sets the stub's files before the connector starts.
    async fn with(prepare: impl FnOnce(&Path)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(dir.path().join("dashboard.json"), busy().to_string()).unwrap();
        prepare(dir.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
        let storage = Storage::at(home.clone()).unwrap();
        let routes_path = dir.path().join("routes.json");
        let pairing = storage
            .pair(
                url,
                "dashboard test".into(),
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
        let cli = stub_cli(dir.path());
        // The first run of a new executable can take seconds on a busy Mac; `ready` waits
        // for the CLI only briefly.
        let _ = std::process::Command::new(&cli)
            .args(["warm", "up"])
            .output();
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
            connector,
            relay,
        }
    }
    /// The calls whose first two words are `first second`.
    fn calls(&self, first: &str, second: &str) -> usize {
        std::fs::read_to_string(self.dir.path().join("argv.log"))
            .unwrap_or_default()
            .lines()
            .filter(|line| {
                let mut words = line.split('\u{1f}');
                words.next() == Some(first) && words.next() == Some(second)
            })
            .count()
    }
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
    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = new_uuid();
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
        let frame = timeout(Duration::from_secs(15), receive_json(&mut self.ws))
            .await
            .expect("the connector answers")
            .unwrap();
        let envelope: Envelope = serde_json::from_value(frame).unwrap();
        let plaintext = self.session.open("d2c", &envelope).unwrap();
        let value: Value =
            serde_json::from_slice(&link::decode_frame(&plaintext).unwrap()).unwrap();
        assert_eq!(value["id"], id, "{value}");
        value
    }
}

#[tokio::test]
async fn ready_announces_the_dashboard_when_the_cli_has_it_and_only_then() {
    let rig = Rig::with(|_| {}).await;
    let mut phone = Phone::connect(&rig.pairing).await;
    assert_eq!(
        phone.ready["features"]["dashboard"], true,
        "{}",
        phone.ready
    );
    // One question answers it with the others, and a yes is believed.
    assert_eq!(phone.ready["features"]["chat"], true);
    assert_eq!(rig.calls("capabilities", "--json"), 1);
    let reply = phone.call("dashboard.get", json!({})).await;
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"], busy());
    assert_eq!(rig.calls("dashboard", "--json"), 1);
    assert_eq!(rig.calls("capabilities", "--json"), 1);

    // A CLI without it, one that says no, one that answers oddly and one from before the
    // question: no `dashboard` in features, the rest as it was, and the method is one this
    // desktop does not have.
    for answer in [
        Some("{\"v\":1,\"chat\":true}"),
        Some("{\"v\":1,\"chat\":true,\"dashboard\":false}"),
        Some("{\"v\":1,\"dashboard\":\"true\"}"),
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
            !features.contains_key("dashboard"),
            "{answer:?}: {features:?}"
        );
        assert!(features.contains_key("deflate"), "{answer:?}");
        let refused = phone.call("dashboard.get", json!({})).await;
        assert_eq!(refused["error"]["code"], "invalid_request", "{refused}");
        assert_eq!(refused["error"]["message"], "unsupported RPC method");
        assert_eq!(rig.calls("dashboard", "--json"), 0, "{answer:?}");
    }
}
