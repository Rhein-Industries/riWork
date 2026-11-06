//! A phone's encrypted connection fans out through registered SSH routes. The
//! SSH executable is a fixture shim; both remote endpoints and RPC dispatch are
//! real, and every file/process belongs to this test's temporary homes.
#![cfg(unix)]

use riwork_remote::{
    client::{HostRecord, HostsFile},
    config::{Pairing, Storage, private_read},
    connector::{connect_registered, receive_json, send_json},
    crypto::{Envelope, ServerHello, Session, accept_server, client_hello, decode, random32},
    link,
    relay::{Relay, Routes},
    ssh::Route,
};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::Path, process::Stdio};
use tokio::{
    process::{Child, Command},
    time::{Duration, sleep, timeout},
};
use uuid::Uuid;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

struct Rig {
    _dir: tempfile::TempDir,
    storage: Storage,
    pairings: Vec<Pairing>,
    peers: Vec<HostRecord>,
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
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("gateway");
        std::fs::create_dir(&home).unwrap();
        let storage = Storage::at(home.clone()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
        let routes = dir.path().join("routes.json");
        let pairings = (0..2)
            .map(|i| {
                storage
                    .pair(
                        url.clone(),
                        format!("phone {i}"),
                        true,
                        &dir.path().join(format!("phone-{i}.json")),
                        Some(&routes),
                    )
                    .unwrap()
            })
            .collect();
        let routes: Routes = private_read(&routes, 1 << 20).unwrap();
        let relay = Relay::new(routes, 16).unwrap();
        let relay = tokio::spawn(async move {
            axum::serve(listener, relay.router()).await.unwrap();
        });

        // Only this connector's children resolve this SSH shim. The command is
        // generated from registered fixture routes, just as a real SSH server
        // receives a remote command from the authenticated account.
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        executable(
            &bin.join("ssh"),
            &format!(
                "#!/usr/bin/python3\nimport os,sys\nwith open({path:?},'a') as f: f.write(str(os.getpid())+'\\n')\nos.execv('/bin/sh',['sh','-c','exec '+sys.argv[-1]])\n",
                path = dir.path().join("ssh-pids").to_string_lossy()
            ),
        );
        let make_cli = |root: &Path, identity: &str, label: &str| {
            std::fs::create_dir_all(root).unwrap();
            let cli = root.join("somtus-fixture");
            executable(
                &cli,
                &format!(
                    r#"#!/usr/bin/python3
import json,sys,pathlib,time
root=pathlib.Path({root:?})
with (root/'calls').open('a') as f: f.write(json.dumps(sys.argv[1:])+'\n')
if sys.argv[1]=='capabilities':
 print(json.dumps({{'v':1,'chat':True,'dashboard':True,'native':{{'v':1,'methods':['Hosts.local','Store.renameProject']}}}}))
elif sys.argv[1:3]==['native','invoke']:
 args=json.load(sys.stdin)
 if args.get('slow'):
  (root/'slow-started').write_text('yes');time.sleep(3);(root/'slow-finished').write_text('yes')
 if sys.argv[3]=='Hosts.local': print(json.dumps({{'v':1,'value':{{'hostId':{identity:?}}}}}))
 else: print(json.dumps({{'v':1,'value':{{'host':{label:?},'args':args}}}}))
elif sys.argv[1:3]==['project','list']:
 print(json.dumps([{{'id':{identity:?},'name':{label:?}}}]))
elif sys.argv[1:3]==['shell','list'] or sys.argv[1:3]==['orchestrator','list']: print('[]')
else: print('{{}}')
"#,
                    root = root.to_string_lossy()
                ),
            );
            cli
        };
        let cli = make_cli(
            &dir.path().join("local"),
            &Uuid::new_v4().to_string(),
            "Gateway",
        );
        let peers = (0..2)
            .map(|i| {
                let host_id = Uuid::new_v4().to_string();
                let peer_home = dir.path().join(format!("peer-{i}"));
                let peer_cli = make_cli(&peer_home, &host_id, &format!("Peer {i}"));
                HostRecord {
                    id: Uuid::new_v4().to_string(),
                    pairing: None,
                    ssh: Some(Route {
                        target: format!("fixture-peer-{i}"),
                        remote_bin: env!("CARGO_BIN_EXE_somtus-remote").into(),
                        somtus: Some(peer_cli.to_string_lossy().into_owned()),
                        home: Some(peer_home.to_string_lossy().into_owned()),
                        host_id,
                        client_id: Uuid::new_v4().to_string(),
                    }),
                    label: format!("Peer {i}"),
                    relay: String::new(),
                    route: String::new(),
                    added_at_unix: 1,
                    allow_insecure_loopback: false,
                }
            })
            .collect::<Vec<_>>();
        HostsFile {
            v: 1,
            hosts: peers.clone(),
        }
        .save(&storage)
        .unwrap();
        let connector = Command::new(env!("CARGO_BIN_EXE_somtus-remote"))
            .env("RIWORK_HOME", &home)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .args(["start", "--somtus"])
            .arg(cli)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            _dir: dir,
            storage,
            pairings,
            peers,
            connector,
            relay,
        }
    }
}

struct Phone {
    ws: Socket,
    session: Session,
}
impl Phone {
    async fn connect(pair: &Pairing) -> Self {
        let (mut ws, online) = timeout(Duration::from_secs(15), async {
            loop {
                if let Ok(connection) =
                    connect_registered(&pair.relay_url, &pair.route_id, "mobile", &pair.relay_token)
                        .await
                {
                    break connection;
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("phone registration");
        if !online {
            let peer = timeout(Duration::from_secs(15), receive_json(&mut ws))
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
        assert_eq!(ready["features"]["hosts"], 1);
        Self { ws, session }
    }
    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        let value = self.next().await;
        assert_eq!(value["id"], id, "{value}");
        value
    }
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
    async fn next(&mut self) -> Value {
        let frame = timeout(Duration::from_secs(25), receive_json(&mut self.ws))
            .await
            .expect("gateway replies")
            .unwrap();
        let envelope: Envelope = serde_json::from_value(frame).unwrap();
        let plaintext = self.session.open("d2c", &envelope).unwrap();
        let value: Value =
            serde_json::from_slice(&link::decode_frame(&plaintext).unwrap()).unwrap();
        value
    }
    async fn open(&mut self, peer: &HostRecord) -> String {
        let value = self
            .call(
                "hosts.open",
                json!({"route_id":peer.id,"host_id":peer.ssh.as_ref().unwrap().host_id}),
            )
            .await;
        assert_eq!(value["ok"], true, "{value}");
        assert_eq!(
            value["result"]["host_id"],
            peer.ssh.as_ref().unwrap().host_id
        );
        assert_eq!(value["result"]["features"]["chat"], true);
        value["result"]["session"].as_str().unwrap().to_owned()
    }
    async fn forwarded(&mut self, session: &str, method: &str, params: Value) -> Value {
        self.call("hosts.call", json!({"session":session,"request_id":Uuid::new_v4().to_string(),"method":method,"params":params})).await
    }
}

#[tokio::test]
async fn paired_phone_reaches_two_ssh_hosts_with_isolated_and_revocable_sessions() {
    let rig = Rig::new().await;
    let mut first = Phone::connect(&rig.pairings[0]).await;
    let mut second = Phone::connect(&rig.pairings[1]).await;
    let listed = first.call("hosts.list", json!({})).await;
    assert_eq!(listed["ok"], true, "{listed}");
    let hosts = listed["result"]["hosts"].as_array().unwrap();
    assert_eq!(hosts.len(), 2);
    for host in hosts {
        let keys = host
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            ["route_id", "host_id", "label", "transport"]
                .into_iter()
                .collect()
        );
    }
    let a = first.open(&rig.peers[0]).await;
    assert_eq!(
        first.open(&rig.peers[0]).await,
        a,
        "repeated open reuses its route"
    );
    let b = first.open(&rig.peers[1]).await;
    for (session, name) in [(&a, "Peer 0"), (&b, "Peer 1")] {
        let answer = first.forwarded(session, "projects.list", json!({})).await;
        assert_eq!(answer["result"]["projects"][0]["name"], name, "{answer}");
    }
    // Possession of another phone's session token is insufficient.
    let stolen = second.forwarded(&a, "projects.list", json!({})).await;
    assert_eq!(stolen["ok"], false, "{stolen}");
    let own = second.open(&rig.peers[0]).await;
    assert_ne!(a, own);
    let write = first
        .forwarded(
            &b,
            "native.invoke",
            json!({"module":"Store","method":"renameProject","args":{}}),
        )
        .await;
    assert_eq!(write["result"]["value"]["host"], "Peer 1", "{write}");
    for method in ["hosts.list", "hosts.call", "pty.open", "link.configure"] {
        let refused = first.forwarded(&a, method, json!({})).await;
        assert_eq!(refused["ok"], false, "must refuse {method}: {refused}");
    }
    assert_eq!(
        first.call("hosts.close", json!({"session":a})).await["ok"],
        true
    );
    assert_eq!(
        first.forwarded(&a, "projects.list", json!({})).await["ok"],
        false
    );
    assert_eq!(
        second.forwarded(&own, "projects.list", json!({})).await["ok"],
        true
    );
    // Removing a route takes effect on an already-open session.
    HostsFile {
        v: 1,
        hosts: vec![rig.peers[0].clone()],
    }
    .save(&rig.storage)
    .unwrap();
    assert_eq!(
        first.forwarded(&b, "projects.list", json!({})).await["ok"],
        false
    );
    // Closing the outer connection also closes its remaining downstream routes.
    second.ws.close(None).await.unwrap();
    let pids: Vec<i32> = std::fs::read_to_string(rig._dir.path().join("ssh-pids"))
        .unwrap()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 3);
    timeout(Duration::from_secs(5), async {
        loop {
            if pids.iter().all(|pid| unsafe { libc::kill(*pid, 0) != 0 }) {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("closing the phone and routes reaps their SSH children");
    let mut reconnected = Phone::connect(&rig.pairings[1]).await;
    assert_eq!(
        reconnected
            .forwarded(&own, "projects.list", json!({}))
            .await["ok"],
        false,
        "a new first-hop session cannot reuse its previous gateway tokens"
    );
    let reopened = reconnected.open(&rig.peers[0]).await;
    assert_ne!(own, reopened);
    rig.storage.revoke(&rig.pairings[1].device_id).unwrap();
    let pid: i32 = std::fs::read_to_string(rig._dir.path().join("ssh-pids"))
        .unwrap()
        .lines()
        .last()
        .unwrap()
        .parse()
        .unwrap();
    timeout(Duration::from_secs(5), async {
        while unsafe { libc::kill(pid, 0) == 0 } {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("revoking a phone closes its downstream SSH session");
}

#[tokio::test]
async fn closing_a_route_during_a_slow_write_leaves_the_gateway_and_other_host_usable() {
    let rig = Rig::new().await;
    let mut phone = Phone::connect(&rig.pairings[0]).await;
    let a = phone.open(&rig.peers[0]).await;
    let b = phone.open(&rig.peers[1]).await;
    let write = phone.send("hosts.call", json!({"session":a,"request_id":Uuid::new_v4().to_string(),"method":"native.invoke","params":{"module":"Store","method":"renameProject","args":{"slow":true}}})).await;
    timeout(Duration::from_secs(5), async {
        while !rig._dir.path().join("peer-0/slow-started").exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("forwarded write started");
    let close = phone.send("hosts.close", json!({"session":a})).await;
    let replies = timeout(Duration::from_secs(2), async {
        [phone.next().await, phone.next().await]
    })
    .await
    .expect("close bypasses a running ordered mutation");
    let closed = replies.iter().find(|reply| reply["id"] == close).unwrap();
    assert_eq!(closed["ok"], true, "{closed}");
    let uncertain = replies.iter().find(|reply| reply["id"] == write).unwrap();
    assert_eq!(
        uncertain["error"]["code"], "delivery_uncertain",
        "{uncertain}"
    );
    let other = phone.forwarded(&b, "projects.list", json!({})).await;
    assert_eq!(other["result"]["projects"][0]["name"], "Peer 1", "{other}");
    assert_eq!(
        phone.call("hosts.close", json!({"session":b})).await["ok"],
        true
    );
    timeout(Duration::from_secs(5), async {
        while !rig._dir.path().join("peer-0/slow-finished").exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the accepted fixture CLI finishes without replay");
    let calls = std::fs::read_to_string(rig._dir.path().join("peer-0/calls")).unwrap();
    assert_eq!(calls.matches("Store.renameProject").count(), 1);
}
