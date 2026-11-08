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
    time::{Duration, Instant, sleep, timeout},
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
 print(json.dumps({{'v':1,'chat':True,'dashboard':True,'native':{{'v':1,'methods':['Hosts.local','Store.renameProject','Store.snapshot']}}}}))
elif sys.argv[1:3]==['native','invoke']:
 args=json.load(sys.stdin)
 if args.get('slow'):
  (root/'slow-started').write_text('yes');end=time.time()+args.get('seconds',30)
  while time.time()<end and not (root/'release').exists(): time.sleep(.02)
  (root/'slow-finished').write_text('yes')
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
            .stderr(std::fs::File::create(dir.path().join("connector.log")).unwrap())
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
    /// One more registered route, whose far end is `script` and not a Somtus: what the
    /// SSH shim runs is its remote command.
    fn add_route(&self, name: &str, script: &str) -> HostRecord {
        let far_end = self._dir.path().join(name);
        executable(&far_end, script);
        let record = HostRecord {
            id: Uuid::new_v4().to_string(),
            pairing: None,
            ssh: Some(Route {
                target: format!("fixture-{name}"),
                remote_bin: far_end.to_string_lossy().into_owned(),
                somtus: None,
                home: None,
                host_id: Uuid::new_v4().to_string(),
                client_id: Uuid::new_v4().to_string(),
            }),
            label: name.into(),
            relay: String::new(),
            route: String::new(),
            added_at_unix: 1,
            allow_insecure_loopback: false,
        };
        let mut file = HostsFile::load(&self.storage).unwrap();
        file.hosts.push(record.clone());
        file.save(&self.storage).unwrap();
        record
    }
    /// The process of the `index`th dial: the shim execs into what it runs.
    fn dialed_pid(&self, index: usize) -> i32 {
        std::fs::read_to_string(self._dir.path().join("ssh-pids"))
            .unwrap()
            .lines()
            .nth(index)
            .unwrap()
            .parse()
            .unwrap()
    }
    fn peer_calls(&self, index: usize) -> String {
        std::fs::read_to_string(self._dir.path().join(format!("peer-{index}/calls"))).unwrap()
    }
    fn connector_log(&self) -> String {
        std::fs::read_to_string(self._dir.path().join("connector.log")).unwrap()
    }
}

/// What a slow fixture call waits for, up to its `seconds`: the test says when it is done.
fn release(root: &Path) {
    std::fs::write(root.join("release"), "").unwrap();
}

/// A host that has gone quiet without closing anything, as one does when it sleeps
/// or drops off the network. It goes on when this is dropped, however the test ends,
/// so that it can notice its client is gone and leave.
struct Stalled(i32);
impl Stalled {
    fn host(pid: i32) -> Self {
        assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
        Self(pid)
    }
}
impl Drop for Stalled {
    fn drop(&mut self) {
        unsafe { libc::kill(self.0, libc::SIGCONT) };
    }
}

/// A far end that lets a dial wait for its handshake limit. Bounded, so that it goes
/// away by itself if a test fails first.
const HANGS: &str = "#!/bin/sh\nexec sleep 50\n";
/// How long the phone gives a plain forwarded request before it ends its whole
/// connection (`RelayClient.timeout`), and `hosts.open`.
const PHONE_TIMER: Duration = Duration::from_secs(15);
const PHONE_OPEN_TIMER: Duration = Duration::from_secs(30);

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
    async fn send_forwarded(&mut self, session: &str, method: &str, params: Value) -> String {
        self.send("hosts.call", json!({"session":session,"request_id":Uuid::new_v4().to_string(),"method":method,"params":params})).await
    }
    async fn send_open(&mut self, peer: &HostRecord) -> String {
        self.send(
            "hosts.open",
            json!({"route_id":peer.id,"host_id":peer.ssh.as_ref().unwrap().host_id}),
        )
        .await
    }
}
fn rename(args: Value) -> Value {
    json!({"module":"Store","method":"renameProject","args":args})
}
/// A read of the desktop's, slow if `args` says so.
fn snapshot(args: Value) -> Value {
    json!({"module":"Store","method":"snapshot","args":args})
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
    // The write runs until this test lets it finish, so any answer before that bypassed it.
    let replies = timeout(Duration::from_secs(10), async {
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
    release(&rig._dir.path().join("peer-0"));
    timeout(Duration::from_secs(10), async {
        while !rig._dir.path().join("peer-0/slow-finished").exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the accepted fixture CLI finishes without replay");
    let calls = std::fs::read_to_string(rig._dir.path().join("peer-0/calls")).unwrap();
    assert_eq!(calls.matches("Store.renameProject").count(), 1);
}

#[tokio::test]
async fn a_slow_write_on_one_host_holds_up_only_that_host() {
    let rig = Rig::new().await;
    let mut phone = Phone::connect(&rig.pairings[0]).await;
    let a = phone.open(&rig.peers[0]).await;
    let b = phone.open(&rig.peers[1]).await;
    let slow = phone
        .send_forwarded(&a, "native.invoke", rename(json!({"slow":true})))
        .await;
    timeout(Duration::from_secs(5), async {
        while !rig._dir.path().join("peer-0/slow-started").exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("forwarded write started");
    // Each of these is answered next, ahead of the write that came first, which runs until
    // this test lets it finish. The reply's id is what `forwarded` and `call` check.
    let other = phone
        .forwarded(&b, "native.invoke", rename(json!({})))
        .await;
    assert_eq!(other["result"]["value"]["host"], "Peer 1", "{other}");
    let own = phone.call("native.invoke", rename(json!({}))).await;
    assert_eq!(own["ok"], true, "{own}");
    assert!(
        !rig._dir.path().join("peer-0/slow-finished").exists(),
        "the other host and the desktop did not wait for it"
    );
    // On its own host a write still waits its turn.
    let second = phone
        .send_forwarded(&a, "native.invoke", rename(json!({})))
        .await;
    sleep(Duration::from_millis(500)).await;
    assert_eq!(
        rig.peer_calls(0).matches("Store.renameProject").count(),
        1,
        "the second write did not start while the first ran"
    );
    release(&rig._dir.path().join("peer-0"));
    assert_eq!(phone.next().await["id"], slow);
    assert!(rig._dir.path().join("peer-0/slow-finished").exists());
    let reply = phone.next().await;
    assert_eq!(reply["id"], second, "{reply}");
    assert_eq!(reply["result"]["value"]["host"], "Peer 0", "{reply}");
}

#[tokio::test]
async fn dials_that_hang_run_side_by_side_and_hold_up_nothing_else() {
    let rig = Rig::new().await;
    let mut phone = Phone::connect(&rig.pairings[0]).await;
    let b = phone.open(&rig.peers[1]).await;
    let hung: Vec<_> = (0..3)
        .map(|n| rig.add_route(&format!("hung-{n}"), HANGS))
        .collect();
    let started = Instant::now();
    let mut opens = Vec::new();
    for route in &hung {
        opens.push(phone.send_open(route).await);
    }
    // Typing on another host and on the desktop go ahead while they dial.
    let write = phone
        .forwarded(&b, "native.invoke", rename(json!({})))
        .await;
    assert_eq!(write["result"]["value"]["host"], "Peer 1", "{write}");
    let own = phone.call("native.invoke", rename(json!({}))).await;
    assert_eq!(own["ok"], true, "{own}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // A dial still counts against the sessions a phone may have.
    let fifth = rig.add_route("hung-more", HANGS);
    let refused = phone
        .call(
            "hosts.open",
            json!({"route_id":fifth.id,"host_id":fifth.ssh.as_ref().unwrap().host_id}),
        )
        .await;
    assert_eq!(refused["error"]["code"], "host_capacity", "{refused}");
    // All three end at the handshake limit, not one after the other.
    for _ in &opens {
        let reply = phone.next().await;
        assert!(
            opens.contains(&reply["id"].as_str().unwrap().to_owned()),
            "{reply}"
        );
        assert_eq!(reply["error"]["code"], "host_timeout", "{reply}");
    }
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn opens_of_one_hung_route_take_turns_inside_the_phones_timer() {
    let rig = Rig::new().await;
    let mut phone = Phone::connect(&rig.pairings[0]).await;
    let hung = rig.add_route("hung", HANGS);
    let started = Instant::now();
    let mut opens = Vec::new();
    for _ in 0..3 {
        opens.push(phone.send_open(&hung).await);
    }
    // One dial at a time: the first ends at the handshake limit, and the phone's timer
    // for the others is running all that while. Run strictly in turn, the third would
    // end at about 45 s, long after that timer ended the phone's whole connection.
    for _ in &opens {
        let reply = phone.next().await;
        assert_eq!(reply["error"]["code"], "host_timeout", "{reply}");
        assert!(
            started.elapsed() < PHONE_OPEN_TIMER,
            "{:?}",
            started.elapsed()
        );
    }
}

#[tokio::test]
async fn a_stalled_host_costs_the_phone_that_route_and_nothing_else() {
    let rig = Rig::new().await;
    let mut phone = Phone::connect(&rig.pairings[0]).await;
    let a = phone.open(&rig.peers[0]).await;
    let b = phone.open(&rig.peers[1]).await;
    let stalled = Stalled::host(rig.dialed_pid(0));
    let started = Instant::now();
    let read = phone.send_forwarded(&a, "projects.list", json!({})).await;
    let write = phone
        .send_forwarded(&a, "native.invoke", rename(json!({})))
        .await;
    // Both in the shared lane, as a read is, and neither known to be one.
    let chunk = phone.send_forwarded(&a, "upload.chunk", json!({})).await;
    let newer = phone
        .send_forwarded(
            &a,
            "native.invoke",
            json!({"module":"Store","method":"aNewerMethod","args":{}}),
        )
        .await;
    let replies = [
        phone.next().await,
        phone.next().await,
        phone.next().await,
        phone.next().await,
    ];
    let waited = started.elapsed();
    // Ahead of the phone's timer, which would end this connection and with it host B.
    assert!(waited < PHONE_TIMER, "{waited:?}");
    assert!(waited >= PHONE_TIMER - Duration::from_secs(3), "{waited:?}");
    let answer = |id: &str| replies.iter().find(|reply| reply["id"] == id).unwrap();
    // Sent, so none may say otherwise. A change may have been carried out.
    for id in [&write, &chunk, &newer] {
        assert_eq!(answer(id)["error"]["code"], "delivery_uncertain");
    }
    assert_eq!(answer(&read)["error"]["code"], "host_unavailable");
    for reply in &replies {
        assert!(!reply.to_string().contains("not sent"), "{reply}");
    }
    let other = phone.forwarded(&b, "projects.list", json!({})).await;
    assert_eq!(other["result"]["projects"][0]["name"], "Peer 1", "{other}");
    // The link itself is given up on soon after, so later calls do not stall again. Its
    // SSH process ends with it: a slow machine may be late to look, and is waited for.
    let host = rig.dialed_pid(0);
    tokio::time::timeout_at(started + Duration::from_secs(40), async {
        while unsafe { libc::kill(host, 0) } == 0 {
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the link to a silent host is given up");
    // The process goes as the link's task ends; a moment for that to be done.
    sleep(Duration::from_millis(500)).await;
    let gone = phone.forwarded(&a, "projects.list", json!({})).await;
    assert_eq!(gone["error"]["code"], "host_session_closed", "{gone}");
    drop(stalled);
}

#[tokio::test]
async fn a_request_still_queued_at_its_deadline_is_answered_and_never_sent() {
    let rig = Rig::new().await;
    let mut phone = Phone::connect(&rig.pairings[0]).await;
    let a = phone.open(&rig.peers[0]).await;
    let stalled = Stalled::host(rig.dialed_pid(0));
    let started = Instant::now();
    // A creation may take far longer than a write, and it holds the host's ordered slot.
    let create = phone.send_forwarded(&a, "shell.create", json!({})).await;
    let write = phone
        .send_forwarded(&a, "native.invoke", rename(json!({})))
        .await;
    // On a longer timer, and sent after the write: it must not reach the host ahead of it.
    let later = phone
        .send_forwarded(
            &a,
            "shell.close",
            json!({"shell_id":Uuid::new_v4().to_string()}),
        )
        .await;
    for id in [&write, &later] {
        let reply = phone.next().await;
        assert_eq!(reply["id"], *id, "{reply}");
        assert_eq!(reply["error"]["code"], "host_timeout", "{reply}");
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not sent"),
            "{reply}"
        );
    }
    let waited = started.elapsed();
    assert!(waited < PHONE_TIMER, "{waited:?}");
    assert!(waited >= PHONE_TIMER - Duration::from_secs(3), "{waited:?}");
    // The host wakes up in time: the route was not given up, the creation is answered.
    drop(stalled);
    assert_eq!(phone.next().await["id"], create);
    let next = phone
        .forwarded(&a, "native.invoke", rename(json!({})))
        .await;
    assert_eq!(next["result"]["value"]["host"], "Peer 0", "{next}");
    assert_eq!(
        rig.peer_calls(0).matches("Store.renameProject").count(),
        1,
        "the queued write never reached the host"
    );
}

#[tokio::test]
async fn a_read_a_live_host_is_too_slow_to_answer_costs_the_read_and_not_the_route() {
    let rig = Rig::new().await;
    let mut phone = Phone::connect(&rig.pairings[0]).await;
    let a = phone.open(&rig.peers[0]).await;
    let started = Instant::now();
    // Longer than the phone gives it. Its host's pongs show that the link is fine.
    let slow = phone
        .send_forwarded(
            &a,
            "native.invoke",
            snapshot(json!({"slow":true,"seconds":14})),
        )
        .await;
    let reply = phone.next().await;
    assert_eq!(reply["id"], slow, "{reply}");
    assert_eq!(reply["error"]["code"], "host_timeout", "{reply}");
    let words = reply["error"]["message"].as_str().unwrap();
    assert!(words.contains("did not answer in time"), "{reply}");
    assert!(!words.contains("not sent"), "{reply}");
    let waited = started.elapsed();
    assert!(waited < PHONE_TIMER, "{waited:?}");
    assert!(waited >= PHONE_TIMER - Duration::from_secs(3), "{waited:?}");
    assert!(rig._dir.path().join("peer-0/slow-started").exists());
    // The phone keeps the route for this code, and the route is still good.
    let next = phone.forwarded(&a, "projects.list", json!({})).await;
    assert_eq!(next["result"]["projects"][0]["name"], "Peer 0", "{next}");
}

#[tokio::test]
async fn a_request_for_this_machine_still_queued_at_its_deadline_is_answered_and_never_run() {
    let rig = Rig::new().await;
    let mut phone = Phone::connect(&rig.pairings[0]).await;
    let a = phone.open(&rig.peers[0]).await;
    // This machine's one ordered slot is taken for longer than a phone gives what waits
    // behind it, and on its timer the phone would end its connection, and every route.
    let slow = phone
        .send("native.invoke", rename(json!({"slow":true,"seconds":16})))
        .await;
    timeout(Duration::from_secs(5), async {
        while !rig._dir.path().join("local/slow-started").exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the slow write started");
    let started = Instant::now();
    let late = phone.send("native.invoke", rename(json!({}))).await;
    let reply = phone.next().await;
    assert_eq!(reply["id"], late, "{reply}");
    assert_eq!(reply["error"]["code"], "host_timeout", "{reply}");
    let words = reply["error"]["message"].as_str().unwrap();
    assert!(words.contains("not sent"), "{reply}");
    let waited = started.elapsed();
    assert!(waited < PHONE_TIMER, "{waited:?}");
    assert!(waited >= PHONE_TIMER - Duration::from_secs(3), "{waited:?}");
    // The route is none the worse, and what was running was left alone.
    let other = phone.send_forwarded(&a, "projects.list", json!({})).await;
    let replies = [phone.next().await, phone.next().await];
    let answer = |id: &str| replies.iter().find(|reply| reply["id"] == id).unwrap();
    assert_eq!(answer(&other)["result"]["projects"][0]["name"], "Peer 0");
    assert_eq!(answer(&slow)["ok"], true);
    assert!(rig._dir.path().join("local/slow-finished").exists());
    let calls = std::fs::read_to_string(rig._dir.path().join("local/calls")).unwrap();
    assert_eq!(
        calls.matches("Store.renameProject").count(),
        1,
        "the queued write never ran"
    );
}

#[tokio::test]
async fn a_failed_open_says_why_in_fixed_words_and_the_gateway_logs_the_code() {
    let rig = Rig::new().await;
    let mut phone = Phone::connect(&rig.pairings[0]).await;
    let refuses = |code: &str| {
        format!(
            "#!/bin/sh\nread hello\necho '{{\"v\":1,\"type\":\"refused\",\"code\":\"{code}\"}}'\necho 'secret from /private/place' >&2\nexit 1\n"
        )
    };
    let ssh_says = |words: &str| {
        format!("#!/bin/sh\necho 'secret from /private/place: {words}' >&2\nexit 255\n")
    };
    let unforwarded = "#!/bin/sh\nread hello\necho '{\"v\":1,\"type\":\"ready\",\"host_id\":\"@HOST@\",\"features\":{}}'\nread more\n";
    let cases = [
        (refuses("identity_changed"), "host_changed"),
        (refuses("session_busy"), "host_busy"),
        (refuses("cli_unsupported"), "host_not_installed"),
        (refuses("cli_missing"), "host_not_installed"),
        (refuses("identity_unavailable"), "host_unavailable"),
        (refuses("home_unavailable"), "host_unavailable"),
        (
            ssh_says("Permission denied (publickey)."),
            "host_login_failed",
        ),
        (
            ssh_says("Host key verification failed."),
            "host_login_failed",
        ),
        (
            ssh_says("Could not resolve hostname here"),
            "host_unreachable",
        ),
        (ssh_says("Connection refused"), "host_unreachable"),
        ("#!/bin/sh\nexit 127\n".to_owned(), "host_not_installed"),
        ("#!/bin/sh\nexit 3\n".to_owned(), "host_unavailable"),
        (unforwarded.to_owned(), "host_unsupported"),
    ];
    let mut failed = Vec::new();
    for (n, (script, code)) in cases.iter().enumerate() {
        let route = rig.add_route(&format!("far-{n}"), script);
        // The one case that answers with an identity has to report the one it is pinned to.
        let host = route.ssh.as_ref().unwrap().host_id.clone();
        executable(
            &rig._dir.path().join(format!("far-{n}")),
            &script.replace("@HOST@", &host),
        );
        let reply = phone
            .call("hosts.open", json!({"route_id":route.id,"host_id":host}))
            .await;
        assert_eq!(reply["error"]["code"], *code, "case {n}: {reply}");
        let text = reply.to_string();
        let remote_bin = route.ssh.as_ref().unwrap().remote_bin.as_str();
        for private in ["secret", "/private", "fixture-far", remote_bin] {
            assert!(!text.contains(private), "case {n}: {text}");
        }
        failed.push(*code);
    }
    let log = rig.connector_log();
    for code in failed {
        assert!(log.contains(code), "{code} in {log}");
    }
    assert!(!log.contains("secret"), "{log}");
}

#[tokio::test]
async fn a_host_that_is_another_installation_is_reported_as_changed() {
    let rig = Rig::new().await;
    let mut phone = Phone::connect(&rig.pairings[0]).await;
    // The route is pinned to an identity this real target does not have.
    let mut moved = rig.peers[0].clone();
    let pin = Uuid::new_v4().to_string();
    moved.ssh.as_mut().unwrap().host_id.clone_from(&pin);
    HostsFile {
        v: 1,
        hosts: vec![moved.clone(), rig.peers[1].clone()],
    }
    .save(&rig.storage)
    .unwrap();
    let reply = phone
        .call("hosts.open", json!({"route_id":moved.id,"host_id":pin}))
        .await;
    assert_eq!(reply["error"]["code"], "host_changed", "{reply}");
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("remove the route") && message.contains("gateway"),
        "{message}"
    );
    assert!(
        !message.contains(&rig._dir.path().to_string_lossy().into_owned()),
        "{message}"
    );
}
