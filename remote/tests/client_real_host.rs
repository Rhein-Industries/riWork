//! The client against the real connector (with a stand-in CLI), as far as the host
//! side goes. The first test runs today: a pairing that is not a Mac's gets no
//! terminal streams. The second needs the host's `pty.*` and `pair --kind desktop`
//! (branch `mac-remote-host`) and is ignored until both are in.
mod client_support;

use client_support::{Bridge, Net, REMOTE, short_dir, wait_for_daemon};
use riwork_remote::{
    client::add_host,
    client_daemon::{serve, socket_path},
    config::{Storage, private_read},
    relay::{Relay, Routes},
};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Connector(Child);
impl Drop for Connector {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn start_connector(host_home: &Path, cli: &Path) -> Connector {
    Connector(
        Command::new(REMOTE)
            .env("RIWORK_HOME", host_home)
            .args(["start", "--riwork"])
            .arg(cli)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
/// A CLI that lists one live shell, says it can `shell attach --exec`, and attaches like a
/// tmux client that draws, reports its size and then echoes like `cat`.
fn cli(dir: &Path, shell: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("real-host-cli");
    let script = r#"#!/bin/sh
case "$1 $2" in
'shell list') printf '[{"id":"@SHELL@","alive":true}]';;
'orchestrator list'|'project list') echo '[]';;
'capabilities --json') printf '{"v":1,"verifies_shell":true,"shell_attach_exec":true}';;
'shell attach')
  # A tmux client starts by drawing with an escape sequence; the host waits for it.
  printf '\033[?25l'; stty size; exec cat;;
esac
"#
    .replace("@SHELL@", shell);
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}
async fn last_end(bridge: &mut Bridge) -> String {
    loop {
        let (kind, payload) = bridge.next().await.expect("an end before the hangup");
        if kind == b'E' {
            return serde_json::from_slice::<serde_json::Value>(&payload).unwrap()["reason"]
                .as_str()
                .unwrap()
                .to_owned();
        }
    }
}

#[tokio::test]
async fn a_pairing_that_is_not_a_macs_cannot_attach_and_is_told_how_to_fix_it() {
    // `pair --protocol 2` without `--kind desktop` is a phone's pairing.
    let net = Net::new(1).await;
    let shell = uuid::Uuid::new_v4().to_string();
    let _connector = start_connector(&net.host_home, &cli(net.dir.path(), &shell));
    let client = Storage::at(net.client_home("client")).unwrap();
    let added = add_host(&client, &net.link(0), Some("Phone-like"), true)
        .await
        .unwrap();
    assert!(added.warning.is_none(), "{:?}", added.warning);
    let id = net.desktop_id();
    let daemon = {
        let (client, id) = (client.clone(), id.clone());
        tokio::spawn(async move { serve(client, &id, Duration::from_secs(300)).await })
    };
    let socket = socket_path(&client, &id).unwrap();
    wait_for_daemon(&socket).await;
    let mut bridge = Bridge::attach(&socket, &shell, 80, 24).await;
    let reason = last_end(&mut bridge).await;
    assert!(reason.contains("--kind desktop"), "{reason}");
    daemon.abort();
}

#[tokio::test]
#[ignore = "needs the host side: `pair --kind desktop` and pty.* (merge branch mac-remote-host, then --ignored)"]
async fn a_mac_attaches_to_a_shell_through_the_real_connector() {
    let dir = short_dir("rwr");
    let host_home = dir.path().join("host");
    std::fs::create_dir(&host_home).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
    let routes_path = dir.path().join("routes.json");
    let paired = Command::new(REMOTE)
        .env("RIWORK_HOME", &host_home)
        .args([
            "pair",
            "--relay",
            &url,
            "--name",
            "A Mac",
            "--allow-insecure-loopback",
        ])
        .args(["--protocol", "2", "--kind", "desktop", "--show-link"])
        .arg("--out")
        .arg(dir.path().join("mac.json"))
        .arg("--relay-routes")
        .arg(&routes_path)
        .output()
        .unwrap();
    assert!(
        paired.status.success(),
        "{}",
        String::from_utf8_lossy(&paired.stderr)
    );
    let link = String::from_utf8_lossy(&paired.stdout)
        .lines()
        .find(|l| l.starts_with("riwork://pair?"))
        .expect("a link")
        .to_owned();
    let routes: Routes = private_read(&routes_path, 1 << 20).unwrap();
    let relay = Relay::new(routes, 16).unwrap();
    let relay = tokio::spawn(async move { axum::serve(listener, relay.router()).await.unwrap() });
    let shell = uuid::Uuid::new_v4().to_string();
    let cli = cli(dir.path(), &shell);
    let connector = start_connector(&host_home, &cli);

    let client = Storage::at({
        let home = dir.path().join("client");
        std::fs::create_dir(&home).unwrap();
        home
    })
    .unwrap();
    let added = add_host(&client, &link, Some("Mac"), true).await.unwrap();
    assert!(added.warning.is_none(), "{:?}", added.warning);
    assert!(
        added.features.unwrap().pty.is_some(),
        "a desktop pairing is offered pty streams"
    );
    let id = Storage::at(host_home.clone())
        .unwrap()
        .config()
        .unwrap()
        .desktop_id;
    let daemon = {
        let (client, id) = (client.clone(), id.clone());
        tokio::spawn(async move { serve(client, &id, Duration::from_secs(300)).await })
    };
    let socket = socket_path(&client, &id).unwrap();
    wait_for_daemon(&socket).await;

    let mut bridge = Bridge::attach(&socket, &shell, 100, 30).await;
    // The stand-in terminal reports its size first: rows, then columns.
    let first = bridge.data_until(b"30 100").await;
    assert!(String::from_utf8_lossy(&first).contains("30 100"));
    bridge.data(b"hello\r").await;
    bridge.data_until(b"hello").await;

    // The connector dies (kill -9): the bridge is told, and when the connector is back the
    // stream is opened again from the start.
    drop(connector);
    assert_eq!(bridge.status().await["state"], "offline");
    let _connector = start_connector(&host_home, &cli);
    assert_eq!(bridge.status().await["state"], "online");
    bridge.data_until(b"30 100").await;
    bridge.data(b"again\r").await;
    bridge.data_until(b"again").await;

    // Ctrl-D ends `cat`, and the stream with it.
    bridge.data(&[4]).await;
    assert_eq!(last_end(&mut bridge).await, "exited");
    daemon.abort();
    relay.abort();
}
