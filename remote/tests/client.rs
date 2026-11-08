//! Using another Mac as a client, against a real relay and the real connector (with
//! a stand-in CLI): pairing from a v2 link, the host registry, the client daemon
//! (`call`, `status`, `ensure`, `socket`, idle exit, `hosts remove`) and what happens
//! when the host goes away, comes back or is revoked. Everything runs in temporary
//! homes with short paths; the client processes are the real binary.
mod client_support;

use client_support::{
    HOSTILE_CODE, HOSTILE_MESSAGE, HOSTILE_RESULT, Net, OtherBuild, REMOTE, eventually, short_dir,
    stub_cli,
};
use riwork_remote::{
    client::{HostRecord, HostsFile},
    client_daemon::{DAEMON_PROTOCOL, daemon_paths, daemon_status, ensure},
    config::Storage,
    ssh::Route,
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::Duration,
};

struct Rig {
    net: Net,
    client_home: PathBuf,
    connector: Option<Child>,
    cli: PathBuf,
}
impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(mut connector) = self.connector.take() {
            let _ = connector.kill();
            let _ = connector.wait();
        }
        // Client processes outlive the tests that started them unless asked.
        if let Ok(id) = std::fs::read_to_string(self.client_home.join("remote/hosts.json"))
            && let Ok(file) = serde_json::from_str::<Value>(&id)
        {
            for host in file["hosts"].as_array().into_iter().flatten() {
                let name = host["id"].as_str().unwrap_or_default();
                let _ = self.remote(&["hosts", "remove", name]).output();
            }
        }
    }
}
impl Rig {
    async fn new(invites: usize) -> Self {
        let net = Net::new(invites).await;
        let client_home = net.client_home("client");
        let shell = uuid::Uuid::new_v4().to_string();
        let cli = stub_cli(net.dir.path(), &shell);
        let mut rig = Self {
            net,
            client_home,
            connector: None,
            cli,
        };
        rig.start_connector();
        rig
    }
    fn start_connector(&mut self) {
        self.connector = Some(
            Command::new(REMOTE)
                .env("RIWORK_HOME", &self.net.host_home)
                .args(["start", "--riwork"])
                .arg(&self.cli)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    fn stop_connector(&mut self) {
        let mut connector = self.connector.take().unwrap();
        connector.kill().unwrap();
        connector.wait().unwrap();
    }
    /// The binary, as a Mac that is a client runs it.
    fn remote(&self, args: &[&str]) -> Command {
        let mut command = Command::new(REMOTE);
        command
            .env("RIWORK_HOME", &self.client_home)
            .args(args)
            .stdin(Stdio::null());
        command
    }
    fn run(&self, args: &[&str]) -> impl std::future::Future<Output = Output> + use<> {
        let mut command = self.remote(args);
        async move {
            tokio::task::spawn_blocking(move || command.output().unwrap())
                .await
                .unwrap()
        }
    }
    async fn add(&self, invite: usize, label: &str) -> Output {
        let link = self.net.link(invite);
        self.run(&[
            "hosts",
            "add",
            "--link",
            &link,
            "--label",
            label,
            "--allow-insecure-loopback",
        ])
        .await
    }
    fn id(&self) -> String {
        self.net.desktop_id()
    }
    fn hosts_file(&self) -> PathBuf {
        self.client_home.join("remote/hosts.json")
    }
    async fn status(&self) -> Value {
        let out = self
            .run(&["status", "--desktop", &self.id(), "--json"])
            .await;
        assert!(out.status.success(), "{}", text(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }
    /// Polls `status` until its state is `state`.
    async fn wait_state(&self, state: &str, seconds: u64) -> Value {
        let end = std::time::Instant::now() + Duration::from_secs(seconds);
        loop {
            let status = self.status().await;
            if status["state"] == state {
                return status;
            }
            assert!(
                std::time::Instant::now() < end,
                "never {state}: last status {status}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn pairing_a_host_stores_an_established_record_and_prints_no_secret() {
    let rig = Rig::new(1).await;
    let invite = rig.net.invites[0].clone();
    let added = rig.add(0, "Studio").await;
    assert!(added.status.success(), "{}", text(&added.stderr));
    let id = rig.id();
    assert_eq!(
        text(&added.stdout).trim(),
        format!("Added host {id} (\"Studio\").")
    );
    // The real connector only offers terminal streams to a device paired with `--kind desktop`.
    assert!(
        text(&added.stderr).is_empty()
            || text(&added.stderr).starts_with("Warning: the host does not offer terminal streams"),
        "{}",
        text(&added.stderr)
    );

    // The record is private and established; the invite secret is gone from it.
    assert_eq!(mode(&rig.hosts_file()), 0o600);
    let file: Value =
        serde_json::from_str(&std::fs::read_to_string(rig.hosts_file()).unwrap()).unwrap();
    assert_eq!(
        (file["v"].as_u64(), file["hosts"].as_array().unwrap().len()),
        (Some(1), 1)
    );
    let host = &file["hosts"][0];
    assert_eq!(host["id"], id.as_str());
    assert_eq!(host["label"], "Studio");
    assert_eq!(host["relay"], rig.net.url.as_str());
    assert_eq!(host["route"], invite.route_id.as_str());
    assert_eq!(host["allow_insecure_loopback"], true);
    assert!(host["added_at_unix"].as_u64().unwrap() > 1_700_000_000);
    let pairing = &host["pairing"];
    assert_eq!(pairing["invite_state"], "established");
    assert!(pairing["root_key"].is_string() && pairing.get("invite_secret").is_none());
    let raw = std::fs::read_to_string(rig.hosts_file()).unwrap();
    assert!(!raw.contains(invite.invite_secret.as_deref().unwrap()));
    // The host consumed the invite exactly once.
    let on_host = rig.net.host.config().unwrap().devices.remove(0);
    assert_eq!(on_host.pairing.invite_state.as_deref(), Some("established"));
    assert_eq!(
        on_host.pairing.root_key,
        pairing["root_key"].as_str().map(str::to_owned)
    );

    // Listing shows the host and nothing secret, in both forms.
    let secrets = [
        pairing["root_key"].as_str().unwrap(),
        pairing["relay_token"].as_str().unwrap(),
        invite.invite_secret.as_deref().unwrap(),
    ];
    for args in [&["hosts", "list"][..], &["hosts", "list", "--json"][..]] {
        let out = rig.run(args).await;
        assert!(out.status.success());
        let shown = text(&out.stdout);
        assert!(shown.contains(&id) && shown.contains("Studio"), "{shown}");
        for secret in secrets {
            assert!(!shown.contains(secret) && !text(&out.stderr).contains(secret));
        }
    }
    let listed: Value =
        serde_json::from_slice(&rig.run(&["hosts", "list", "--json"]).await.stdout).unwrap();
    assert_eq!(listed[0]["id"], id.as_str());
    assert_eq!(listed[0].as_object().unwrap().len(), 7, "{listed}");
    assert!(listed[0].get("ssh").is_none(), "{listed}");

    // Pairing the same host again is refused before the network is used.
    let again = rig.add(0, "Again").await;
    assert!(!again.status.success());
    assert!(
        text(&again.stderr).contains("already added"),
        "{}",
        text(&again.stderr)
    );
    for secret in secrets {
        assert!(!text(&again.stderr).contains(secret));
    }
}

#[tokio::test]
async fn a_link_can_be_read_from_standard_input_to_keep_it_out_of_the_process_list() {
    use std::io::Write;
    let rig = Rig::new(1).await;
    let link = rig.net.link(0);
    let mut command = rig.remote(&[
        "hosts",
        "add",
        "--link",
        "-",
        "--label",
        "Studio",
        "--allow-insecure-loopback",
        "--json",
    ]);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("  {link}\n").as_bytes())
        .unwrap();
    let out = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    // `--json` prints the host as `hosts list --json` shows it.
    let added: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(added["id"], rig.id().as_str());
    assert_eq!(added["label"], "Studio");
    assert!(added.get("pairing").is_none() && !text(&out.stdout).contains("root_key"));
    let listed: Value =
        serde_json::from_slice(&rig.run(&["hosts", "list", "--json"]).await.stdout).unwrap();
    assert_eq!(listed[0], added);

    // Two lines are not one link.
    let mut command = rig.remote(&["hosts", "add", "--link", "-", "--allow-insecure-loopback"]);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{link}\n{link}\n").as_bytes())
        .unwrap();
    let out = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert!(!out.status.success());
}

#[tokio::test]
async fn a_used_or_malformed_link_is_refused_and_leaves_no_host() {
    let rig = Rig::new(1).await;
    assert!(rig.add(0, "First").await.status.success());
    // The invite was consumed: after removing the host the same link must not work again.
    let removed = rig.run(&["hosts", "remove", &rig.id()]).await;
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    let replay = rig.add(0, "Replay").await;
    assert!(!replay.status.success());
    assert!(
        text(&replay.stderr).contains("invite_replay"),
        "{}",
        text(&replay.stderr)
    );
    let listed = rig.run(&["hosts", "list", "--json"]).await;
    assert_eq!(text(&listed.stdout).trim(), "[]");

    for bad in [
        "not a link",
        "https://example.com/pair?v=2&data=AA",
        "somtus://pair?v=2",
        "somtus://pair?v=2&data=%41%41",
        "somtus://other?v=2&data=AA",
        "somtus://pair?v=3&data=AA",
    ] {
        let out = rig
            .run(&["hosts", "add", "--link", bad, "--allow-insecure-loopback"])
            .await;
        assert!(!out.status.success(), "{bad}");
        assert!(!text(&out.stderr).is_empty());
    }
    // A phone's protocol 1 link is not for this.
    let v1 = rig
        .run(&["hosts", "add", "--link", "somtus://pair?v=1&data=AA"])
        .await;
    assert!(
        text(&v1.stderr).contains("protocol 1"),
        "{}",
        text(&v1.stderr)
    );
    assert!(
        !rig.hosts_file().exists()
            || text(&rig.run(&["hosts", "list", "--json"]).await.stdout).trim() == "[]"
    );
}

#[tokio::test]
async fn call_and_status_go_through_a_client_process_that_ensure_starts_once() {
    let rig = Rig::new(1).await;
    assert!(rig.add(0, "Studio").await.status.success());
    let id = rig.id();

    let socket = rig.run(&["client", "socket", "--desktop", &id]).await;
    assert!(socket.status.success());
    let socket = PathBuf::from(text(&socket.stdout).trim());
    assert!(socket.is_absolute() && socket.starts_with(rig.client_home.join("remote/run")));
    assert!(!socket.exists(), "`client socket` starts nothing");

    let ensured = rig.run(&["client", "ensure", "--desktop", &id]).await;
    assert!(ensured.status.success(), "{}", text(&ensured.stderr));
    assert!(socket.exists());
    // A socket only its owner can use, in a directory only its owner can enter.
    assert_eq!(mode(&socket), 0o600);
    assert_eq!(mode(socket.parent().unwrap()), 0o700);
    // Idempotent, also when repeated at once: still one client process.
    let (a, b) = tokio::join!(
        rig.run(&["client", "ensure", "--desktop", &id]),
        rig.run(&["client", "ensure", "--desktop", &id])
    );
    assert!(a.status.success() && b.status.success());
    let log = std::fs::read_to_string(socket.with_extension("log")).unwrap();
    assert_eq!(log.matches("serving \"Studio\"").count(), 1, "{log}");

    let status = rig.wait_state("online", 10).await;
    assert_eq!(status["label"], "Studio");
    assert_eq!(status["daemon"], 2, "{status}");
    let status = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let status = rig.status().await;
            if status["rtt_ms"].is_u64() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("a round trip time in the status");
    assert!(status["rtt_ms"].as_u64().unwrap() < 5000, "{status}");
    let human = rig.run(&["status", "--desktop", &id]).await;
    assert!(
        text(&human.stdout).starts_with("online ("),
        "{}",
        text(&human.stdout)
    );

    let listed = rig.run(&["call", "--desktop", &id, "projects.list"]).await;
    assert!(listed.status.success(), "{}", text(&listed.stderr));
    let result: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(result, json!({"projects": []}));

    // The host's errors come through with their code; so do bad inputs.
    let unknown = rig.run(&["call", "--desktop", &id, "no.such.method"]).await;
    assert!(!unknown.status.success());
    assert!(
        text(&unknown.stderr).contains("invalid_request"),
        "{}",
        text(&unknown.stderr)
    );
    let bad_params = rig
        .run(&["call", "--desktop", &id, "projects.list", "--params", "[1]"])
        .await;
    assert!(!bad_params.status.success());
    let bad_json = rig
        .run(&["call", "--desktop", &id, "projects.list", "--params", "{"])
        .await;
    assert!(!bad_json.status.success());
    let with_params = rig
        .run(&[
            "call",
            "--desktop",
            &id,
            "projects.list",
            "--params",
            r#"{"x":1}"#,
        ])
        .await;
    assert!(
        !with_params.status.success(),
        "the host refuses fields it does not know"
    );
    let nobody = rig
        .run(&[
            "call",
            "--desktop",
            "11111111-2222-4333-8444-555555555555",
            "projects.list",
        ])
        .await;
    assert!(
        text(&nobody.stderr).contains("no host"),
        "{}",
        text(&nobody.stderr)
    );
}

#[tokio::test]
#[ignore = "slow: real connector and client processes; stops and restarts the host and waits out reconnect backoff"]
async fn the_client_process_reconnects_when_the_host_returns_and_stays_offline_when_revoked() {
    let mut rig = Rig::new(1).await;
    assert!(rig.add(0, "Studio").await.status.success());
    let id = rig.id();
    rig.wait_state("online", 10).await;

    rig.stop_connector();
    let down = rig.wait_state("offline", 10).await;
    assert!(down["since"].as_u64().unwrap() > 1_700_000_000);
    assert!(down["reason"].as_str().unwrap().len() > 3, "{down}");
    let refused = rig.run(&["call", "--desktop", &id, "projects.list"]).await;
    assert!(!refused.status.success());
    assert!(
        text(&refused.stderr).contains("offline"),
        "{}",
        text(&refused.stderr)
    );

    rig.start_connector();
    rig.wait_state("online", 20).await;
    let again = rig.run(&["call", "--desktop", &id, "projects.list"]).await;
    assert!(again.status.success(), "{}", text(&again.stderr));

    // Revoked on the host: its connector drops the device, and nothing answers.
    let device = rig.net.host.config().unwrap().devices[0]
        .pairing
        .device_id
        .clone();
    rig.net.host.revoke(&device).unwrap();
    let revoked = rig.wait_state("offline", 15).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let still = rig.status().await;
    assert_eq!(still["state"], "offline", "{revoked} then {still}");
}

#[tokio::test]
#[ignore = "slow: real connector and client processes; stops and restarts the host and waits out reconnect backoff"]
async fn watch_streams_state_changes() {
    let mut rig = Rig::new(1).await;
    assert!(rig.add(0, "Studio").await.status.success());
    let id = rig.id();
    rig.wait_state("online", 10).await;
    let mut watching = rig
        .remote(&["status", "--desktop", &id, "--watch", "--json"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = watching.stdout.take().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let _ = tx.send(serde_json::from_str(&line).unwrap());
        }
    });
    async fn next(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Value>, state: &str) -> Value {
        let end = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let line = tokio::time::timeout_at(end, rx.recv())
                .await
                .unwrap_or_else(|_| panic!("no {state} line"))
                .expect("the watcher ended");
            if line["state"] == state {
                return line;
            }
        }
    }
    next(&mut rx, "online").await;
    rig.stop_connector();
    next(&mut rx, "offline").await;
    rig.start_connector();
    next(&mut rx, "online").await;
    watching.kill().unwrap();
    watching.wait().unwrap();
}

#[tokio::test]
#[ignore = "slow: real client process; waits out a 2 s idle timeout (about 5 s)"]
async fn the_client_process_exits_when_idle_and_a_second_one_does_not_start() {
    let rig = Rig::new(1).await;
    assert!(rig.add(0, "Studio").await.status.success());
    let id = rig.id();
    let socket = PathBuf::from(
        text(
            &rig.run(&["client", "socket", "--desktop", &id])
                .await
                .stdout,
        )
        .trim(),
    );
    let mut serving = rig
        .remote(&["client", "serve", "--desktop", &id, "--idle-seconds", "2"])
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // A client keeps it alive however long this takes (the socket file is there a moment
    // before it takes connections, so connecting is retried).
    let client = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match tokio::net::UnixStream::connect(&socket).await {
                Ok(client) => break client,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await
    .expect("the client process takes connections on its socket");
    let held = std::time::Instant::now();
    // Another one for the same host steps aside at once and does not disturb the first.
    let second = rig
        .run(&["client", "serve", "--desktop", &id, "--idle-seconds", "2"])
        .await;
    assert!(second.status.success(), "{}", text(&second.stderr));
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(socket.exists());
    assert!(
        serving.try_wait().unwrap().is_none(),
        "not idle while a client is connected ({:?})",
        held.elapsed()
    );
    // Without clients it goes after the idle time.
    drop(client);
    let begun = std::time::Instant::now();
    let status = tokio::task::spawn_blocking(move || serving.wait().unwrap())
        .await
        .unwrap();
    assert!(status.success());
    assert!(begun.elapsed() >= Duration::from_secs(1));
    assert!(begun.elapsed() < Duration::from_secs(10));
    assert!(!socket.exists(), "the socket goes with the process");
}

#[tokio::test]
async fn removing_a_host_stops_its_client_process_and_forgets_its_credentials() {
    let rig = Rig::new(1).await;
    assert!(rig.add(0, "Studio").await.status.success());
    let id = rig.id();
    rig.wait_state("online", 10).await;
    let socket = PathBuf::from(
        text(
            &rig.run(&["client", "socket", "--desktop", &id])
                .await
                .stdout,
        )
        .trim(),
    );
    assert!(socket.exists());
    let removed = rig.run(&["hosts", "remove", &id]).await;
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    // The record held the only copy of what the host knows this Mac by, and the host
    // goes on trusting it.
    let device = rig.net.host.config().unwrap().devices[0]
        .pairing
        .device_id
        .clone();
    let printed = text(&removed.stdout);
    assert!(
        printed.starts_with(&format!("Removed host {id} (\"Studio\").\n")),
        "{printed}"
    );
    assert!(
        printed.contains(&format!("somtus remote revoke {device}")),
        "{printed}"
    );
    assert!(!printed.contains("add-ssh"), "{printed}");
    eventually(10, "the client process to quit", || !socket.exists()).await;
    let raw = std::fs::read_to_string(rig.hosts_file()).unwrap();
    assert!(!raw.contains("root_key"), "{raw}");
    let call = rig.run(&["call", "--desktop", &id, "projects.list"]).await;
    assert!(!call.status.success());
    assert!(text(&call.stderr).contains("no host"));
    let twice = rig.run(&["hosts", "remove", &id]).await;
    assert!(!twice.status.success());
}

/// A relay record's id, relay and route have to agree with its pairing, so there is
/// nothing `update-ssh` could change, not even by accident.
#[tokio::test]
async fn update_ssh_refuses_a_relay_host_and_leaves_the_registry_as_it_was() {
    let rig = Rig::new(1).await;
    assert!(rig.add(0, "Studio").await.status.success());
    let id = rig.id();
    let before = std::fs::read(rig.hosts_file()).unwrap();
    for option in [["--target", "elsewhere"], ["--label", "Renamed"]] {
        let out = rig
            .run(&["hosts", "update-ssh", &id, option[0], option[1]])
            .await;
        assert!(!out.status.success(), "{option:?}");
        assert!(
            text(&out.stderr).contains("relay"),
            "{option:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(std::fs::read(rig.hosts_file()).unwrap(), before);
    }
}

/// With both kinds on one list, each line has to say which it is. The JSON of a relay
/// host stays as it was: only an SSH route has an `ssh`.
#[tokio::test]
async fn hosts_list_tells_transports_apart() {
    let rig = Rig::new(1).await;
    assert!(rig.add(0, "Studio").await.status.success());
    let id = rig.id();
    let storage = Storage::at(rig.client_home.clone()).unwrap();
    let mut file = HostsFile::load(&storage).unwrap();
    let (route, host_id) = (
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    file.hosts.push(HostRecord {
        id: route.clone(),
        pairing: None,
        ssh: Some(Route {
            target: "build-server".into(),
            remote_bin: "/opt/somtus remote".into(),
            somtus: None,
            home: Some("/srv/somtus home".into()),
            host_id: host_id.clone(),
            client_id: uuid::Uuid::new_v4().to_string(),
        }),
        label: "Build server".into(),
        relay: String::new(),
        route: String::new(),
        added_at_unix: 1,
        allow_insecure_loopback: false,
    });
    file.save(&storage).unwrap();
    let out = rig.run(&["hosts", "list"]).await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let shown = text(&out.stdout);
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines.len(), 2, "{shown}");
    for word in [id.as_str(), "relay", rig.net.url.as_str(), "Studio"] {
        assert!(lines[0].contains(word), "{word}: {shown}");
    }
    for word in [route.as_str(), "ssh", host_id.as_str(), "Build server"] {
        assert!(lines[1].contains(word), "{word}: {shown}");
    }
    assert!(
        !shown.contains("/opt") && !shown.contains("/srv"),
        "{shown}"
    );
    let listed: Value =
        serde_json::from_slice(&rig.run(&["hosts", "list", "--json"]).await.stdout).unwrap();
    assert!(listed[0].get("ssh").is_none(), "{listed}");
    assert_eq!(
        listed[1]["ssh"],
        json!({"target":"build-server","remote_bin":"/opt/somtus remote","home":"/srv/somtus home"})
    );
}

#[tokio::test]
async fn a_client_process_quits_when_its_host_leaves_the_registry_even_with_a_client_on_it() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rig = Rig::new(1).await;
    assert!(rig.add(0, "Studio").await.status.success());
    let id = rig.id();
    rig.wait_state("online", 10).await;
    let socket = PathBuf::from(
        text(
            &rig.run(&["client", "socket", "--desktop", &id])
                .await
                .stdout,
        )
        .trim(),
    );
    // A connected client keeps it from idling out, and nothing tells it of this save.
    let mut watching = tokio::net::UnixStream::connect(&socket).await.unwrap();
    watching.write_all(b"{\"op\":\"watch\"}\n").await.unwrap();
    HostsFile {
        v: 1,
        hosts: vec![],
    }
    .save(&Storage::at(rig.client_home.clone()).unwrap())
    .unwrap();
    eventually(10, "the client process to quit", || !socket.exists()).await;
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), watching.read_to_end(&mut rest))
        .await
        .expect("the client sees the connection end")
        .unwrap();
}

// The other build runs in this test process, so these need a thread to serve it on while
// the last of `Rig` waits for a command in its `Drop`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_of_an_older_build_makes_way_for_this_one() {
    let rig = Rig::new(1).await;
    assert!(rig.add(0, "Studio").await.status.success());
    let id = rig.id();
    let old = OtherBuild::start(&rig.client_home, &id, None, true);
    let ensured = rig.run(&["client", "ensure", "--desktop", &id]).await;
    assert!(ensured.status.success(), "{}", text(&ensured.stderr));
    assert_eq!(old.shutdowns(), 1);
    let status = rig.wait_state("online", 10).await;
    assert_eq!(status["label"], "Studio", "{status}");
    assert_eq!(status["daemon"], 2, "{status}");
    let listed = rig.run(&["call", "--desktop", &id, "projects.list"]).await;
    assert!(listed.status.success(), "{}", text(&listed.stderr));
    assert_eq!(old.shutdowns(), 1, "and it is not asked again");
}

/// A route in a registry written by hand, for what needs a record and no connection.
fn a_registered_route(home: &Path) -> (Storage, String) {
    std::fs::create_dir_all(home).unwrap();
    let storage = Storage::at(home.to_path_buf()).unwrap();
    let route = uuid::Uuid::new_v4().to_string();
    HostsFile {
        v: 1,
        hosts: vec![HostRecord {
            id: route.clone(),
            pairing: None,
            ssh: Some(Route {
                target: "never-dialed".into(),
                remote_bin: "/never".into(),
                somtus: None,
                home: None,
                host_id: uuid::Uuid::new_v4().to_string(),
                client_id: uuid::Uuid::new_v4().to_string(),
            }),
            label: "Studio".into(),
            relay: String::new(),
            route: String::new(),
            added_at_unix: 1,
            allow_insecure_loopback: false,
        }],
    }
    .save(&storage)
    .unwrap();
    (storage, route)
}

/// What `ensure` would start need not be this build: a bundle can be rolled back under
/// running bridges, and the number in their code says nothing about the file. Replacing a
/// daemon with one that is no newer gains nothing, and bridges of the newer build would go
/// on doing it to each other.
#[tokio::test]
async fn a_daemon_is_not_told_to_quit_for_a_file_that_is_no_newer() {
    for (case, body) in [
        (
            "refuses the question, as an older build does",
            Some("echo \"error: unrecognized subcommand '$2'\" >&2\nexit 2"),
        ),
        ("the same number", Some("echo 1")),
        ("a lower number", Some("echo 0")),
        ("not a number", Some("echo soon")),
        ("a number, and a failure", Some("echo 9\nexit 1")),
        ("no such file", None),
    ] {
        let dir = short_dir("rwc");
        let home = dir.path().join("client");
        let (storage, route) = a_registered_route(&home);
        let exe = dir.path().join("somtus-remote");
        if let Some(body) = body {
            std::fs::write(&exe, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let running = OtherBuild::start(&home, &route, Some(1), true);
        let socket = ensure(&storage, &route, &exe)
            .await
            .unwrap_or_else(|error| panic!("{case}: {error:#}"));
        let paths = daemon_paths(&storage, &route).unwrap();
        assert_eq!(socket, paths.socket, "{case}");
        assert_eq!(running.shutdowns(), 0, "{case}");
        assert!(!paths.log.exists(), "{case}: nothing else was started");
        assert_eq!(
            daemon_status(&paths.socket).await.unwrap()["label"],
            "Other build",
            "{case}"
        );
    }
}

/// The question is asked with the lock held, so it has to end.
#[tokio::test]
async fn a_file_that_does_not_answer_is_not_waited_for() {
    let dir = short_dir("rwc");
    let home = dir.path().join("client");
    let (storage, route) = a_registered_route(&home);
    let exe = dir.path().join("somtus-remote");
    std::fs::write(&exe, "#!/bin/sh\nexec sleep 120\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    let running = OtherBuild::start(&home, &route, Some(1), true);
    let begun = std::time::Instant::now();
    ensure(&storage, &route, &exe).await.unwrap();
    assert!(
        begun.elapsed() < Duration::from_secs(60),
        "{:?}",
        begun.elapsed()
    );
    assert_eq!(running.shutdowns(), 0);
}

/// `ensure` asks this of the file before it runs it for real, whatever state the home is in.
#[tokio::test]
async fn the_file_says_its_protocol_and_needs_no_home_to_say_it() {
    let dir = short_dir("rwc");
    let not_a_directory = dir.path().join("file");
    std::fs::write(&not_a_directory, "").unwrap();
    let asked = tokio::process::Command::new(REMOTE)
        .env("RIWORK_HOME", &not_a_directory)
        .args(["client", "protocol"])
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(asked.status.success(), "{}", text(&asked.stderr));
    assert_eq!(text(&asked.stdout), format!("{DAEMON_PROTOCOL}\n"));
    // It is for `ensure`, not for people.
    let help = tokio::process::Command::new(REMOTE)
        .env("RIWORK_HOME", &not_a_directory)
        .args(["client", "--help"])
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(help.status.success(), "{}", text(&help.stderr));
    assert!(
        !text(&help.stdout).contains("protocol"),
        "{}",
        text(&help.stdout)
    );
}

/// A host's error is the host's words. This is the one place they go straight to a terminal,
/// where an escape sequence can retitle it, repaint it or set the clipboard.
#[tokio::test]
async fn call_prints_a_hosts_error_without_its_control_characters() {
    let dir = short_dir("rwc");
    let home = dir.path().join("client");
    let (_, route) = a_registered_route(&home);
    let _daemon = OtherBuild::start(&home, &route, Some(DAEMON_PROTOCOL), true);
    let out = tokio::process::Command::new(REMOTE)
        .env("RIWORK_HOME", &home)
        .args(["call", "--desktop", &route, "projects.list"])
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    let stderr = text(&out.stderr);
    // Sanity: the answer it was given holds what must not get through.
    assert!(HOSTILE_CODE.contains('\u{1b}') && HOSTILE_MESSAGE.contains('\u{1b}'));
    assert!(stderr.contains("run this"), "{stderr:?}");
    assert!(stderr.contains("code"), "{stderr:?}");
    for control in ['\u{1b}', '\u{7}', '\u{9b}'] {
        assert!(!stderr.contains(control), "{stderr:?}");
    }
}

/// A result is the host's words too. JSON escapes the C0 controls by itself, and not DEL or
/// the C1 controls, which a terminal acts on just the same.
#[tokio::test]
async fn call_prints_a_hosts_result_without_its_control_characters() {
    let dir = short_dir("rwc");
    let home = dir.path().join("client");
    let (_, route) = a_registered_route(&home);
    let _daemon = OtherBuild::start(&home, &route, Some(DAEMON_PROTOCOL), true);
    let out = tokio::process::Command::new(REMOTE)
        .env("RIWORK_HOME", &home)
        .args(["call", "--desktop", &route, "shells.list"])
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        !stdout
            .chars()
            .any(|c| c.is_control() && c != '\n' || ('\u{7f}'..='\u{9f}').contains(&c)),
        "{stdout:?}"
    );
    // The same JSON all the same.
    let printed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(printed, json!({"name": HOSTILE_RESULT}), "{stdout:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_of_the_same_or_a_newer_build_is_left_alone() {
    for protocol in [2, 3] {
        let rig = Rig::new(1).await;
        assert!(rig.add(0, "Studio").await.status.success());
        let id = rig.id();
        let other = OtherBuild::start(&rig.client_home, &id, Some(protocol), true);
        let ensured = rig.run(&["client", "ensure", "--desktop", &id]).await;
        assert!(ensured.status.success(), "{}", text(&ensured.stderr));
        let status = rig.status().await;
        assert_eq!(status["label"], "Other build", "{status}");
        assert_eq!(status["daemon"], protocol, "{status}");
        assert_eq!(other.shutdowns(), 0, "protocol {protocol}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_of_an_older_build_that_will_not_go_is_used_rather_than_failing() {
    let rig = Rig::new(1).await;
    assert!(rig.add(0, "Studio").await.status.success());
    let id = rig.id();
    let stubborn = OtherBuild::start(&rig.client_home, &id, Some(1), false);
    let begun = std::time::Instant::now();
    let ensured = rig.run(&["client", "ensure", "--desktop", &id]).await;
    assert!(ensured.status.success(), "{}", text(&ensured.stderr));
    assert!(
        begun.elapsed() >= Duration::from_secs(3),
        "it had its chance"
    );
    assert!(
        begun.elapsed() < Duration::from_secs(9),
        "{:?}",
        begun.elapsed()
    );
    assert_eq!(stubborn.shutdowns(), 1);
    let storage = Storage::at(rig.client_home.clone()).unwrap();
    let paths = daemon_paths(&storage, &id).unwrap();
    let status = daemon_status(&paths.socket).await.unwrap();
    assert_eq!(status["label"], "Other build", "{status}");
    assert!(!paths.log.exists(), "nothing else was started");
}
