use super::*;
use tokio::io::duplex;

#[cfg(unix)]
#[tokio::test]
async fn cancelled_before_dispatch_is_never_written_to_ssh() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let calls = tmp.path().join("wire");
    let mut process = Process::new("python3");
    process
        .arg("-u")
        .arg("-c")
        .arg(format!(
            r#"
import sys,json,pathlib
hello=json.loads(sys.stdin.readline())
print(json.dumps({{'v':1,'type':'ready','host_id':{host:?}}}),flush=True)
for line in sys.stdin:
 frame=json.loads(line)
 if frame['type']=='ping': print('{{"v":1,"type":"pong"}}',flush=True)
 else:
  with pathlib.Path({path:?}).open('a') as f: f.write(frame['method']+'\n')
  print(json.dumps({{'v':1,'type':'response','id':frame['id'],'ok':True,'result':{{}}}}),flush=True)
"#,
            host = route.host_id,
            path = calls.to_string_lossy()
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let (dialed, ready) = dial_command(&route, Some(&route.host_id), process)
        .await
        .unwrap();
    let (state, _) = watch::channel(LinkState::Online { rtt_ms: None });
    let (conn, mut current) = watch::channel(None);
    let task = tokio::spawn(async move {
        connected(dialed, ready, LinkTiming::default(), &state, &conn, &mut 0).await
    });
    current.changed().await.unwrap();
    let connection = current.borrow().clone().unwrap();
    let (reply, rx) = oneshot::channel();
    drop(rx);
    connection
        .tx
        .send(Command::Request {
            id: "cancelled".into(),
            method: "mutation".into(),
            params: json!({}),
            reply,
        })
        .await
        .unwrap();
    let (reply, rx) = oneshot::channel();
    connection
        .tx
        .send(Command::Request {
            id: "live".into(),
            method: "read".into(),
            params: json!({}),
            reply,
        })
        .await
        .unwrap();
    timeout(Duration::from_secs(3), rx)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    connection
        .tx
        .send(Command::Reset("test finished".into()))
        .await
        .unwrap();
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read_to_string(calls).unwrap(), "read\n");
}

fn route() -> Route {
    Route {
        target: "user@fixture".into(),
        remote_bin: "/fixture/Somtus's App/somtus-remote".into(),
        somtus: Some("/fixture/Somtus's App/somtus".into()),
        home: Some("/fixture/home $(literal)".into()),
        host_id: uuid::Uuid::new_v4().to_string(),
        client_id: uuid::Uuid::new_v4().to_string(),
    }
}

#[test]
fn ssh_arguments_preserve_host_verification_and_quote_remote_paths() {
    let mut route = route();
    route.validate().unwrap();
    let command = route.command();
    let args: Vec<_> = command
        .as_std()
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    assert!(args.iter().any(|a| a == "StrictHostKeyChecking=yes"));
    assert!(args.iter().any(|a| a == "BatchMode=yes"));
    assert_eq!(args[0], "-T");
    assert_eq!(args[args.len() - 2], "user@fixture");
    assert!(
        args.last()
            .unwrap()
            .contains("RIWORK_HOME='/fixture/home $(literal)'")
    );
    assert!(args.last().unwrap().contains("Somtus'\\''s App"));
    for invalid in ["-oProxyCommand=bad", "host\ncommand", "user host", ""] {
        route.target = invalid.into();
        assert!(route.validate().is_err());
    }
}

#[test]
fn ssh_registry_has_no_fake_pairing_and_rejects_unpinned_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::at(tmp.path().into()).unwrap();
    let mut record = HostRecord {
        id: uuid::Uuid::new_v4().to_string(),
        pairing: None,
        ssh: Some(route()),
        label: "Fixture".into(),
        relay: String::new(),
        route: String::new(),
        added_at_unix: 1,
        allow_insecure_loopback: false,
    };
    record.validate().unwrap();
    let summary = record.summary();
    assert_eq!(summary.transport, "ssh");
    assert_eq!(
        summary.host_id,
        Some(record.ssh.as_ref().unwrap().host_id.clone())
    );
    HostsFile {
        v: 1,
        hosts: vec![record.clone()],
    }
    .save(&storage)
    .unwrap();
    let loaded = HostsFile::load(&storage).unwrap();
    assert_eq!(loaded.hosts[0].ssh, record.ssh);
    assert!(!serde_json::to_string(&record).unwrap().contains("pairing"));
    record.ssh.as_mut().unwrap().host_id.clear();
    assert!(record.validate().is_err());
}

#[test]
fn a_summary_shows_how_a_route_connects_but_not_its_client_id() {
    let route = route();
    let record = HostRecord {
        id: uuid::Uuid::new_v4().to_string(),
        pairing: None,
        ssh: Some(route.clone()),
        label: "Fixture".into(),
        relay: String::new(),
        route: String::new(),
        added_at_unix: 1,
        allow_insecure_loopback: false,
    };
    let summary = serde_json::to_value(record.summary()).unwrap();
    assert_eq!(
        summary["ssh"],
        json!({
            "target": route.target,
            "remote_bin": route.remote_bin,
            "somtus": route.somtus,
            "home": route.home,
        })
    );
    assert!(!summary.to_string().contains(&route.client_id));
    // A summary reads back as what it was: a desktop reading the list may be another build.
    let again: HostSummary = serde_json::from_value(summary).unwrap();
    assert_eq!(again, record.summary());
    let bare = Route {
        somtus: None,
        home: None,
        ..route
    };
    let summary = serde_json::to_value(
        HostRecord {
            ssh: Some(bare),
            ..record
        }
        .summary(),
    )
    .unwrap();
    assert_eq!(summary["ssh"].as_object().unwrap().len(), 2, "{summary}");
}

#[test]
fn the_command_for_a_route_is_quoted_and_says_only_what_differs_from_a_plain_add() {
    let route = route();
    assert_eq!(
        route.summary().add_command(Some("-Bob's Mac")),
        "somtus-remote hosts add-ssh --target='user@fixture' \
         --remote-bin='/fixture/Somtus'\\''s App/somtus-remote' \
         --somtus='/fixture/Somtus'\\''s App/somtus' \
         --home='/fixture/home $(literal)' --label='-Bob'\\''s Mac'"
    );
    let plain = Route {
        remote_bin: DEFAULT_REMOTE_BIN.into(),
        somtus: None,
        home: None,
        ..route
    };
    assert_eq!(
        plain.summary().add_command(None),
        "somtus-remote hosts add-ssh --target='user@fixture'"
    );
}

/// `add-ssh` asks for an absolute home now. A route registered before it did has to keep
/// loading: `validate` runs on every load, and one failure refuses the whole registry.
#[test]
fn a_home_that_is_not_absolute_is_refused_only_as_a_new_value() {
    let mut route = route();
    for home in ["relative/home", "~/home", "$HOME/home"] {
        route.home = Some(home.into());
        route.validate().unwrap();
        assert!(check_new_home(home).is_err(), "{home}");
    }
    check_new_home("/absolute/home").unwrap();
}

/// Nor does it stand in the way of changing anything else about such a route.
#[tokio::test]
async fn a_route_with_a_home_from_before_that_rule_can_still_be_renamed() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::at(tmp.path().into()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    HostsFile {
        v: 1,
        hosts: vec![HostRecord {
            id: id.clone(),
            pairing: None,
            ssh: Some(Route {
                home: Some("relative/home".into()),
                ..route()
            }),
            label: "Fixture".into(),
            relay: String::new(),
            route: String::new(),
            added_at_unix: 1,
            allow_insecure_loopback: false,
        }],
    }
    .save(&storage)
    .unwrap();
    let change = RouteChange {
        label: Some("Renamed".into()),
        ..RouteChange::default()
    };
    let updated = update(&storage, &id, change).await.unwrap();
    assert!(updated.saved && !updated.route_changed);
    assert_eq!(updated.host.label, "Renamed");
    let loaded = HostsFile::load(&storage).unwrap();
    assert_eq!(
        loaded.hosts[0].ssh.as_ref().unwrap().home.as_deref(),
        Some("relative/home")
    );
}

#[test]
fn a_default_label_always_passes_as_a_label() {
    for target in [
        "a",
        "alice@host",
        &"x".repeat(64),
        &"x".repeat(65),
        &"ü".repeat(512),
    ] {
        let label = default_label(target);
        check_label(&label).unwrap();
        assert!(target.starts_with(&label), "{target}");
        assert_eq!(label.chars().count(), target.chars().count().min(64));
    }
}

#[tokio::test]
async fn partial_line_survives_select_cancellation_and_oversized_frame_is_bounded() {
    let (mut sender, receiver) = duplex(4096);
    let mut receiver = BufReader::new(receiver);
    let mut partial = Vec::new();
    sender.write_all(b"{\"v\":").await.unwrap();
    assert!(
        timeout(
            Duration::from_millis(5),
            read_json(&mut receiver, &mut partial, 128)
        )
        .await
        .is_err()
    );
    assert_eq!(partial, b"{\"v\":");
    sender.write_all(b"1}\n").await.unwrap();
    assert_eq!(
        read_json(&mut receiver, &mut partial, 128).await.unwrap(),
        Some(json!({"v":1}))
    );
    sender.write_all(&[b'x'; 200]).await.unwrap();
    assert!(read_json(&mut receiver, &mut partial, 128).await.is_err());
    assert!(partial.len() <= 129);
}

#[test]
fn ready_requires_stable_identity_and_protocol_before_requests() {
    let route = route();
    let ready = json!({"v":1,"type":"ready","host_id":route.host_id});
    validate_ready(&ready, Some(&route.host_id)).unwrap();
    assert!(validate_ready(&ready, Some(&uuid::Uuid::new_v4().to_string())).is_err());
    assert!(validate_ready(&json!({"v":2,"type":"ready","host_id":route.host_id}), None).is_err());
}

#[cfg(unix)]
fn cli(tmp: &std::path::Path, host: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = tmp.join("fake-cli");
    let body = format!(
        r#"#!/usr/bin/env python3
import sys,json,time,pathlib
root=pathlib.Path({root:?})
with (root/'calls').open('a') as f: f.write(' '.join(sys.argv[1:])+'\n')
if sys.argv[1]=='capabilities':
 print(json.dumps({{'v':1,'native':{{'v':1,'methods':['Hosts.local','Store.renameProject']}}}}))
elif sys.argv[1:3]==['native','invoke']:
 if sys.argv[3]=='Hosts.local': print(json.dumps({{'v':1,'value':{{'hostId':{host:?}}}}}))
 else:
  (root/'started').write_text('yes');time.sleep(.15);(root/'finished').write_text('yes')
  print(json.dumps({{'v':1,'value':{{'ok':True}}}}))
else: print('{{}}')
"#,
        root = tmp.to_string_lossy(),
        host = host
    );
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    script
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_rejects_changed_host_before_dispatching_queued_mutations() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let cli = cli(tmp.path(), &route.host_id);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let (mut client, server) = duplex(16384);
    let (read, write) = tokio::io::split(server);
    let task = tokio::spawn(serve_io(storage, cli, read, write));
    write_json(&mut client,&json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":uuid::Uuid::new_v4().to_string(),"refused":1})).await.unwrap();
    write_json(&mut client,&json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":"native.invoke","params":{"module":"Store","method":"renameProject","args":{}}})).await.unwrap();
    let error = task.await.unwrap().unwrap_err();
    assert!(
        error.to_string().contains("identity changed"),
        "{error:#}; calls={:?}",
        std::fs::read_to_string(tmp.path().join("calls"))
    );
    let calls = std::fs::read_to_string(tmp.path().join("calls")).unwrap();
    assert!(!calls.contains("renameProject"));
    // The client is told why, in fixed words, and nothing follows.
    let mut reader = BufReader::new(client);
    let mut partial = Vec::new();
    assert_eq!(
        read_json(&mut reader, &mut partial, MAX_LINE)
            .await
            .unwrap(),
        refused("identity_changed")
    );
    assert_eq!(
        read_json(&mut reader, &mut partial, MAX_LINE)
            .await
            .unwrap(),
        None
    );
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_uses_existing_rpc_and_finishes_accepted_mutation_after_disconnect() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let cli = cli(tmp.path(), &route.host_id);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let (client, server) = duplex(16384);
    let (read, write) = tokio::io::split(server);
    let task = tokio::spawn(serve_io(storage, cli, read, write));
    let (reader, mut writer) = tokio::io::split(client);
    let mut reader = BufReader::new(reader);
    let mut partial = Vec::new();
    write_json(
        &mut writer,
        &json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":route.host_id}),
    )
    .await
    .unwrap();
    let ready = read_json(&mut reader, &mut partial, MAX_LINE)
        .await
        .unwrap()
        .unwrap();
    validate_ready(&ready, Some(&route.host_id)).unwrap();
    assert!(ready["features"]["pty"].is_object());
    write_json(&mut writer, &json!({"v":1,"type":"ping"}))
        .await
        .unwrap();
    assert_eq!(
        read_json(&mut reader, &mut partial, MAX_LINE)
            .await
            .unwrap()
            .unwrap()["type"],
        "pong"
    );
    write_json(&mut writer,&json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":"native.invoke","params":{"module":"Store","method":"renameProject","args":{}}})).await.unwrap();
    timeout(Duration::from_secs(3), async {
        while !tmp.path().join("started").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    writer.shutdown().await.unwrap();
    drop(writer);
    drop(reader);
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(tmp.path().join("finished").exists());
    let calls = std::fs::read_to_string(tmp.path().join("calls")).unwrap();
    assert_eq!(
        calls.matches("Store.renameProject").count(),
        1,
        "no mutation replay"
    );
}

#[cfg(unix)]
struct Endpoint {
    reader: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    partial: Vec<u8>,
    task: tokio::task::JoinHandle<Result<()>>,
}
#[cfg(unix)]
impl Endpoint {
    /// A session that was sent `hello`, and the first thing it answered; `None`
    /// when it said nothing before ending.
    async fn start(
        storage: Storage,
        cli: PathBuf,
        hello: Value,
        idle: IdleLimits,
    ) -> (Self, Option<Value>) {
        let (client, server) = duplex(16384);
        let (read, write) = tokio::io::split(server);
        let task = tokio::spawn(serve_limited(storage, cli, read, write, idle));
        let (reader, writer) = tokio::io::split(client);
        let mut endpoint = Self {
            reader: BufReader::new(reader),
            writer,
            partial: Vec::new(),
            task,
        };
        write_json(&mut endpoint.writer, &hello).await.unwrap();
        let first = timeout(
            HANDSHAKE,
            read_json(&mut endpoint.reader, &mut endpoint.partial, MAX_LINE),
        )
        .await
        .expect("the session answers its hello")
        .unwrap();
        (endpoint, first)
    }
    async fn open(storage: Storage, cli: PathBuf, route: &Route, scope: &str) -> Self {
        let hello = json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":route.host_id,"session_id":scope});
        let (endpoint, ready) = Self::start(storage, cli, hello, IDLE_LIMITS).await;
        let ready = ready.unwrap();
        assert_eq!(ready["session_id"], scope);
        assert_eq!(ready["features"]["forwarded_sessions"], 1);
        assert!(ready["features"]["pty"].is_null());
        assert!(ready["features"]["upload"].is_object());
        assert_eq!(ready["features"]["chat"], true);
        endpoint
    }
    async fn call(&mut self, method: &str, params: Value) -> Value {
        write_json(&mut self.writer, &json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":method,"params":params})).await.unwrap();
        timeout(
            Duration::from_secs(5),
            read_json(&mut self.reader, &mut self.partial, MAX_LINE),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    }
    async fn ping(&mut self) {
        write_json(&mut self.writer, &json!({"v":1,"type":"ping"}))
            .await
            .unwrap();
        let pong = timeout(
            Duration::from_secs(5),
            read_json(&mut self.reader, &mut self.partial, MAX_LINE),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(pong["type"], "pong");
    }
    async fn close(mut self) {
        self.writer.shutdown().await.unwrap();
        drop(self.writer);
        drop(self.reader);
        timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn forwarded_stdio_isolates_phones_preserves_uploads_and_clears_viewport_leases() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::at(tmp.path().into()).unwrap();
    let route = route();
    let cli = tmp.path().join("session-cli");
    let shell = uuid::Uuid::new_v4().to_string();
    let calls = tmp.path().join("session-calls");
    std::fs::write(&cli, format!(r#"#!/usr/bin/env python3
import sys,json
with open({calls:?},'a') as f: f.write(json.dumps(sys.argv[1:])+'\n')
if sys.argv[1]=='capabilities': print(json.dumps({{'v':1,'chat':True,'shell_paste':True,'native':{{'v':1,'methods':['Hosts.local']}}}}))
elif sys.argv[1:3]==['native','invoke']: print(json.dumps({{'v':1,'value':{{'hostId':{host:?}}}}}))
elif sys.argv[1:3]==['shell','list']: print(json.dumps([{{'id':{shell:?},'alive':True,'mode':'terminal'}}]))
elif sys.argv[1:3]==['orchestrator','list']: print('[]')
else: print('{{}}')
"#, calls=calls.to_string_lossy(), host=route.host_id)).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let one = uuid::Uuid::new_v4().to_string();
    let two = uuid::Uuid::new_v4().to_string();
    let mut first = Endpoint::open(storage.clone(), cli.clone(), &route, &one).await;
    let mut other_phone = route.clone();
    other_phone.client_id = uuid::Uuid::new_v4().to_string();
    let mut second = Endpoint::open(storage.clone(), cli.clone(), &other_phone, &two).await;
    assert!(
        storage
            .dir
            .join(format!("ssh-client-{}.json", route.client_id))
            .is_file()
    );
    assert!(
        storage
            .dir
            .join(format!("ssh-client-{}.json", other_phone.client_id))
            .is_file()
    );
    // A reconnect cannot overlap a still-active process with the same input and
    // upload principal. It waits a little for the old one, then is told it is
    // busy before ready, without having started its CLI.
    let calls_before = std::fs::read_to_string(&calls).unwrap();
    let started = Instant::now();
    let (denied, frame) = Endpoint::start(
        storage.clone(),
        cli.clone(),
        json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":route.host_id,"session_id":uuid::Uuid::new_v4().to_string(),"refused":1}),
        IDLE_LIMITS,
    )
    .await;
    assert_eq!(frame, refused("session_busy"));
    assert!(denied.task.await.unwrap().is_err());
    assert!(started.elapsed() >= FORWARDED_LOCK_WAIT);
    assert!(started.elapsed() < HANDSHAKE);
    assert_eq!(std::fs::read_to_string(&calls).unwrap(), calls_before);
    assert_eq!(
        first
            .call("pty.open", json!({"shell_id":shell,"columns":80,"rows":24}))
            .await["ok"],
        false
    );
    for endpoint in [&mut first, &mut second] {
        let response = endpoint
            .call(
                "shell.resize",
                json!({"shell_id":shell,"columns":80,"rows":24}),
            )
            .await;
        assert_eq!(response["ok"], true, "{response}");
    }
    // The timer renews both independent leases while their phones are heard from.
    timeout(Duration::from_secs(5), async {
        loop {
            let text = std::fs::read_to_string(&calls).unwrap();
            if text
                .lines()
                .filter(|line| line.starts_with("[\"shell\", \"resize\""))
                .count()
                >= 4
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let upload = uuid::Uuid::new_v4().to_string();
    let begin = json!({"upload":upload,"shell_id":shell,"name":"example.txt","size":3,"sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"});
    assert_eq!(first.call("upload.begin", begin.clone()).await["ok"], true);
    // Another phone cannot address this phone's upload, even with its UUID.
    assert_eq!(
        second
            .call(
                "upload.chunk",
                json!({"upload":upload,"offset":0,"data":"YWJj"})
            )
            .await["error"]["code"],
        "not_found"
    );
    assert_eq!(second.call("upload.begin", begin).await["ok"], true);
    let mut files = Vec::new();
    for endpoint in [&mut first, &mut second] {
        assert_eq!(
            endpoint
                .call(
                    "upload.chunk",
                    json!({"upload":upload,"offset":0,"data":"YWJj"})
                )
                .await["ok"],
            true
        );
        let finish = endpoint
            .call("upload.finish", json!({"upload":upload}))
            .await;
        assert_eq!(finish["ok"], true, "{finish}");
        let path = PathBuf::from(finish["result"]["path"].as_str().unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
        files.push(path);
    }
    assert_ne!(files[0], files[1]);
    let batch = uuid::Uuid::new_v4().to_string();
    let paste = json!({"shell_id":shell,"batch":batch,"uploads":[upload]});
    assert_eq!(
        first.call("shell.paste", paste.clone()).await["result"]["status"],
        "sent"
    );
    first.close().await;
    // A terminal/application may consume pasted paths after the phone sleeps.
    assert!(files[0].exists());
    assert!(files[1].exists());
    let mut resumed = Endpoint::open(
        storage.clone(),
        cli,
        &route,
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    assert_eq!(
        resumed
            .call("upload.finish", json!({"upload":upload}))
            .await["result"]["path"],
        files[0].to_string_lossy().as_ref()
    );
    assert_eq!(
        resumed.call("shell.paste", paste).await["result"]["status"],
        "duplicate"
    );
    resumed.close().await;
    second.close().await;
    assert!(files[0].exists());
    assert!(files[1].exists());
    let args: Vec<Value> = std::fs::read_to_string(calls)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        args.iter()
            .filter(|args| args[0] == "shell" && args[1] == "paste")
            .count(),
        1
    );
    let leases: std::collections::HashSet<_> = args
        .iter()
        .filter(|args| args[1] == "resize")
        .map(|args| args[6].as_str().unwrap())
        .collect();
    let cleared: std::collections::HashSet<_> = args
        .iter()
        .filter(|args| args[1] == "resize-clear")
        .map(|args| args[6].as_str().unwrap())
        .collect();
    assert_eq!(leases.len(), 2);
    assert_eq!(leases, cleared);
}

#[cfg(unix)]
#[tokio::test]
async fn duplicate_outstanding_request_id_is_refused_without_losing_original_reply() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let wire = tmp.path().join("wire");
    let mut process = Process::new("python3");
    process.arg("-u").arg("-c").arg(format!(r#"
import sys,json,time
hello=json.loads(sys.stdin.readline())
print(json.dumps({{'v':1,'type':'ready','host_id':{host:?}}}),flush=True)
for line in sys.stdin:
 frame=json.loads(line)
 if frame['type']=='ping': print('{{"v":1,"type":"pong"}}',flush=True)
 else:
  with open({wire:?},'a') as f: f.write(frame['id']+'\n')
  time.sleep(.2)
  print(json.dumps({{'v':1,'type':'response','id':frame['id'],'ok':True,'result':{{'original':True}}}}),flush=True)
"#, host=route.host_id, wire=wire.to_string_lossy())).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let (dialed, ready) = dial_command(&route, Some(&route.host_id), process)
        .await
        .unwrap();
    let (state, _) = watch::channel(LinkState::Online { rtt_ms: None });
    let (conn, mut current) = watch::channel(None);
    let task = tokio::spawn(async move {
        connected(dialed, ready, LinkTiming::default(), &state, &conn, &mut 0).await
    });
    current.changed().await.unwrap();
    let connection = current.borrow().clone().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let (reply, original) = oneshot::channel();
    connection
        .tx
        .send(Command::Request {
            id: id.clone(),
            method: "mutation".into(),
            params: json!({}),
            reply,
        })
        .await
        .unwrap();
    let (reply, duplicate) = oneshot::channel();
    connection
        .tx
        .send(Command::Request {
            id: id.clone(),
            method: "mutation".into(),
            params: json!({}),
            reply,
        })
        .await
        .unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(3), duplicate)
            .await
            .unwrap()
            .unwrap(),
        Err(CallError::Invalid(_))
    ));
    assert_eq!(
        timeout(Duration::from_secs(3), original)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .response["result"]["original"],
        true
    );
    connection
        .tx
        .send(Command::Reset("test finished".into()))
        .await
        .unwrap();
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read_to_string(wire).unwrap(), format!("{id}\n"));
}

#[cfg(unix)]
fn refused(code: &str) -> Option<Value> {
    Some(json!({"v":1,"type":"refused","code":code}))
}
#[cfg(unix)]
fn direct_hello(route: &Route) -> Value {
    json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":route.host_id,"refused":1})
}
/// A CLI that names `methods` and runs `local`, one line of Python, for
/// `native invoke Hosts.local`.
#[cfg(unix)]
fn cli_with_local(tmp: &std::path::Path, methods: &str, local: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = tmp.join("local-cli");
    let body = format!(
        r#"#!/usr/bin/env python3
import sys,json,pathlib
root=pathlib.Path({root:?})
with (root/'calls').open('a') as f: f.write(' '.join(sys.argv[1:])+'\n')
if sys.argv[1]=='capabilities':
 print(json.dumps({{'v':1,'native':{{'v':1,'methods':{methods}}}}}))
elif sys.argv[1:3]==['native','invoke']:
 {local}
else: print('{{}}')
"#,
        root = tmp.to_string_lossy()
    );
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    script
}
#[cfg(unix)]
const LOCAL_FAILS: &str =
    "print(json.dumps({'v':1,'error':{'code':'cli_error','message':'tmux is required'}}))";
#[cfg(unix)]
const HOSTS_LOCAL: &str = "['Hosts.local']";
#[cfg(unix)]
fn calls_of(tmp: &std::path::Path) -> String {
    std::fs::read_to_string(tmp.join("calls")).unwrap_or_default()
}

#[test]
fn the_saved_identity_is_read_but_never_created_or_repaired() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::at(tmp.path().into()).unwrap();
    let path = tmp.path().join("host-identity");
    assert_eq!(saved_identity(&storage), SavedIdentity::Missing);
    assert!(!path.exists());
    let id = uuid::Uuid::new_v4().to_string();
    for text in [id.clone(), format!("{id}\n"), format!("  {id}  \n")] {
        std::fs::write(&path, text).unwrap();
        assert_eq!(saved_identity(&storage), SavedIdentity::Valid(id.clone()));
    }
    for text in [
        String::new(),
        "invalid".to_owned(),
        id.to_uppercase(),
        "x".repeat(4096),
    ] {
        std::fs::write(&path, &text).unwrap();
        assert_eq!(saved_identity(&storage), SavedIdentity::Corrupt, "{text}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
    std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
    assert_eq!(saved_identity(&storage), SavedIdentity::Corrupt);
    // Unreadable is not the same as absent: `Hosts.local` must not get to mint one.
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(saved_identity(&storage), SavedIdentity::Corrupt);
}

/// Neither end may wait on what is not a file: a client looks while it holds the registry
/// lock, and a host's session loop stands still while it looks.
#[cfg(unix)]
#[test]
fn a_saved_identity_that_is_not_a_regular_file_is_corrupt_and_waits_for_nobody() {
    use std::os::unix::ffi::OsStrExt;
    fn without_waiting(storage: &Storage) -> SavedIdentity {
        let (sent, got) = std::sync::mpsc::channel();
        let storage = storage.clone();
        std::thread::spawn(move || {
            let _ = sent.send(saved_identity(&storage));
        });
        got.recv_timeout(Duration::from_secs(10))
            .expect("nothing writes to this FIFO, and the read must not wait for it")
    }
    fn fifo(path: &std::path::Path) {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::at(tmp.path().into()).unwrap();
    let path = tmp.path().join("host-identity");
    fifo(&path);
    assert_eq!(without_waiting(&storage), SavedIdentity::Corrupt);
    std::fs::remove_file(&path).unwrap();
    // Through a symlink too, which the rest of this read still follows.
    let elsewhere = tmp.path().join("elsewhere");
    fifo(&elsewhere);
    std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
    assert_eq!(without_waiting(&storage), SavedIdentity::Corrupt);
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("/dev/zero", &path).unwrap();
    assert_eq!(without_waiting(&storage), SavedIdentity::Corrupt);
    std::fs::remove_file(&path).unwrap();
    // What was good before still is.
    let id = uuid::Uuid::new_v4().to_string();
    let saved = tmp.path().join("saved");
    std::fs::write(&saved, &id).unwrap();
    std::os::unix::fs::symlink(&saved, &path).unwrap();
    assert_eq!(without_waiting(&storage), SavedIdentity::Valid(id));
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(tmp.path().join("nothing"), &path).unwrap();
    assert_eq!(without_waiting(&storage), SavedIdentity::Missing);
}

#[cfg(unix)]
#[tokio::test]
async fn an_inventory_larger_than_one_frame_still_reaches_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let local = format!(
        "print(json.dumps({{'v':1,'value':{{'hostId':'{}','sessions':['x'*400]*1500}}}}))",
        route.host_id
    );
    let cli = cli_with_local(tmp.path(), HOSTS_LOCAL, &local);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let (endpoint, ready) = Endpoint::start(storage, cli, direct_hello(&route), IDLE_LIMITS).await;
    let ready = ready.expect("ready");
    assert_eq!(ready["type"], "ready", "{ready}");
    assert_eq!(ready["host_id"], route.host_id.as_str());
    endpoint.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_saved_identity_answers_without_asking_for_the_inventory() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    std::fs::write(
        tmp.path().join("host-identity"),
        format!("{}\n", route.host_id),
    )
    .unwrap();
    let cli = cli_with_local(tmp.path(), HOSTS_LOCAL, LOCAL_FAILS);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let (endpoint, ready) = Endpoint::start(storage, cli, direct_hello(&route), IDLE_LIMITS).await;
    let ready = ready.expect("ready");
    assert_eq!(ready["type"], "ready", "{ready}");
    assert_eq!(ready["host_id"], route.host_id.as_str());
    assert!(!calls_of(tmp.path()).contains("Hosts.local"));
    endpoint.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_corrupt_saved_identity_is_refused_and_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let path = tmp.path().join("host-identity");
    std::fs::write(&path, "not an identity").unwrap();
    let local = format!(
        "print(json.dumps({{'v':1,'value':{{'hostId':'{}'}}}}))",
        route.host_id
    );
    let cli = cli_with_local(tmp.path(), HOSTS_LOCAL, &local);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let (endpoint, frame) = Endpoint::start(storage, cli, direct_hello(&route), IDLE_LIMITS).await;
    assert_eq!(frame, refused("identity_unavailable"));
    assert!(endpoint.task.await.unwrap().is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not an identity");
    assert!(!calls_of(tmp.path()).contains("Hosts.local"));
}

#[cfg(unix)]
#[tokio::test]
async fn an_identity_minted_before_the_inventory_failed_is_used() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let local = format!(
        "(root/'host-identity').write_text('{}');{LOCAL_FAILS}",
        route.host_id
    );
    let cli = cli_with_local(tmp.path(), HOSTS_LOCAL, &local);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let (endpoint, ready) = Endpoint::start(storage, cli, direct_hello(&route), IDLE_LIMITS).await;
    let ready = ready.expect("ready");
    assert_eq!(ready["type"], "ready", "{ready}");
    assert_eq!(ready["host_id"], route.host_id.as_str());
    endpoint.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn with_no_identity_to_be_had_the_refusal_says_why() {
    let unsupported = "print(json.dumps({'v':1,'error':{'code':'invalid_request','message':'unsupported native method'}}))";
    let no_id = "print(json.dumps({'v':1,'value':{}}))";
    for (methods, local, code) in [
        // An older CLI does not name the method at all.
        ("[]", LOCAL_FAILS, "cli_unsupported"),
        (HOSTS_LOCAL, unsupported, "cli_unsupported"),
        (HOSTS_LOCAL, LOCAL_FAILS, "identity_unavailable"),
        (HOSTS_LOCAL, no_id, "identity_unavailable"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let route = route();
        let cli = cli_with_local(tmp.path(), methods, local);
        let storage = Storage::at(tmp.path().into()).unwrap();
        let (endpoint, frame) =
            Endpoint::start(storage, cli, direct_hello(&route), IDLE_LIMITS).await;
        assert_eq!(frame, refused(code), "{local}");
        assert!(endpoint.task.await.unwrap().is_err(), "{local}");
        assert!(!tmp.path().join("host-identity").exists());
    }
}

/// The identity no longer needs the CLI, so this would otherwise look like a
/// healthy host whose every request fails.
#[cfg(unix)]
#[tokio::test]
async fn a_cli_that_cannot_start_is_refused_even_with_a_saved_identity() {
    for saved in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let route = route();
        if saved {
            std::fs::write(tmp.path().join("host-identity"), &route.host_id).unwrap();
        }
        let storage = Storage::at(tmp.path().into()).unwrap();
        let (endpoint, frame) = Endpoint::start(
            storage,
            tmp.path().join("no-such-cli"),
            direct_hello(&route),
            IDLE_LIMITS,
        )
        .await;
        assert_eq!(frame, refused("cli_missing"), "saved identity: {saved}");
        assert!(endpoint.task.await.unwrap().is_err());
    }
}

/// `body` is what a run of the CLI does when it is asked `capabilities`.
#[cfg(unix)]
fn cli_asked(tmp: &std::path::Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = tmp.join("asked-cli");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ncase \"$1\" in\ncapabilities) {body};;\n*) echo '{{}}';;\nesac\n"),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    script
}

/// macOS kills a signed binary that was overwritten in place, and a person told to update
/// Somtus because of that is sent the wrong way. A CLI that ran and refused the question is
/// the one this answer is for.
#[cfg(unix)]
#[tokio::test]
async fn a_cli_killed_by_a_signal_is_not_taken_for_one_too_old() {
    for (dies, code) in [
        ("kill -9 $$", "identity_unavailable"),
        // The same death, with a shell between this and the binary.
        ("exit 137", "identity_unavailable"),
        ("exit 2", "cli_unsupported"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let route = route();
        let cli = cli_asked(tmp.path(), dies);
        let storage = Storage::at(tmp.path().into()).unwrap();
        let (endpoint, frame) =
            Endpoint::start(storage, cli, direct_hello(&route), IDLE_LIMITS).await;
        assert_eq!(frame, refused(code), "{dies}");
        assert!(endpoint.task.await.unwrap().is_err(), "{dies}");
    }
    // With an identity saved the session starts, and it does not call the CLI old either.
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    std::fs::write(tmp.path().join("host-identity"), &route.host_id).unwrap();
    let cli = cli_asked(tmp.path(), "kill -9 $$");
    let storage = Storage::at(tmp.path().into()).unwrap();
    let (mut endpoint, ready) =
        Endpoint::start(storage, cli, direct_hello(&route), IDLE_LIMITS).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    let reply = endpoint
        .call(
            "native.invoke",
            json!({"module":"Hosts","method":"local","args":{}}),
        )
        .await;
    assert_eq!(reply["error"]["code"], "cli_error", "{reply}");
    let reply = endpoint.call("chats.list", json!({})).await;
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(
        !reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("update Somtus"),
        "{reply}"
    );
    endpoint.close().await;
}

/// The first run after an update can take seconds. With nothing saved `Hosts.local` asked
/// with all the time a call gets, so this must not be worth less with an identity saved.
#[cfg(unix)]
#[tokio::test]
async fn a_slow_cli_announces_the_same_features_with_a_saved_identity_as_without_one() {
    async fn features(saved: bool) -> Value {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let route = route();
        if saved {
            std::fs::write(tmp.path().join("host-identity"), &route.host_id).unwrap();
        }
        let script = tmp.path().join("slow-cli");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
case "$1 $2" in
capabilities*) sleep 4; echo '{{"v":1,"chat":true,"tabs":true,"dashboard":true,"native":{{"v":1,"methods":["Hosts.local"]}}}}';;
'native invoke') echo '{{"v":1,"value":{{"hostId":"{host}"}}}}';;
*) echo '[]';;
esac
"#,
                host = route.host_id,
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let storage = Storage::at(tmp.path().into()).unwrap();
        let (endpoint, ready) =
            Endpoint::start(storage, script, direct_hello(&route), IDLE_LIMITS).await;
        let ready = ready.expect("ready");
        assert_eq!(ready["type"], "ready", "{ready}");
        endpoint.close().await;
        ready["features"].clone()
    }
    let (saved, unsaved) = tokio::join!(features(true), features(false));
    assert_eq!(saved["chat"], true, "{saved}");
    assert_eq!(saved["tabs"], true, "{saved}");
    assert_eq!(saved["dashboard"], true, "{saved}");
    assert_eq!(
        saved["native"]["methods"],
        json!(["Hosts.local"]),
        "{saved}"
    );
    assert_eq!(saved, unsaved);
}

/// The client's limit is on the whole handshake, so what the forwarded lock took comes off.
/// The first question runs longer than what is left of it, and only its answer says yes.
#[cfg(unix)]
#[tokio::test]
async fn the_question_gets_only_what_the_lock_wait_left_of_the_handshake() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    std::fs::write(tmp.path().join("host-identity"), &route.host_id).unwrap();
    let asked = tmp.path().join("asked");
    let cli = cli_asked(
        tmp.path(),
        &format!(
            r#"if [ -e '{asked}' ]; then echo '{{"v":1}}'; else : > '{asked}'; sleep 9; echo '{{"v":1,"chat":true}}'; fi"#,
            asked = asked.display()
        ),
    );
    let storage = Storage::at(tmp.path().into()).unwrap();
    let lock = storage
        .lock_wait(
            &format!("ssh-forwarded-{}.lock", route.client_id),
            Duration::ZERO,
        )
        .unwrap();
    let held = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        drop(lock);
    });
    let hello = json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":route.host_id,"session_id":uuid::Uuid::new_v4().to_string(),"refused":1});
    let (endpoint, ready) = Endpoint::start(storage, cli, hello, IDLE_LIMITS).await;
    held.await.unwrap();
    let ready = ready.expect("ready");
    assert_eq!(ready["type"], "ready", "{ready}");
    assert!(ready["features"].get("chat").is_none(), "{ready}");
    endpoint.close().await;
}

/// The client's limit runs from before `ssh` connects, and connecting and logging in can
/// take seconds of it. A CLI that never answers costs the session its features, not the
/// session: what is asked after the handshake's own question gets only what that one left.
#[cfg(unix)]
#[tokio::test]
async fn a_cli_that_never_answers_leaves_ssh_most_of_the_clients_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    std::fs::write(tmp.path().join("host-identity"), &route.host_id).unwrap();
    let asked = tmp.path().join("asked");
    let cli = cli_asked(
        tmp.path(),
        &format!("echo >> '{}'; exec sleep 60", asked.display()),
    );
    let storage = Storage::at(tmp.path().into()).unwrap();
    let started = Instant::now();
    let (endpoint, ready) = Endpoint::start(storage, cli, direct_hello(&route), IDLE_LIMITS).await;
    let took = started.elapsed();
    let ready = ready.expect("ready");
    assert_eq!(ready["type"], "ready", "{ready}");
    assert!(ready["features"].get("chat").is_none(), "{ready}");
    // The handshake's own question had all its time, and ssh has at least 3 s of the rest.
    assert!(took >= CLI_QUESTION_WAIT, "{took:?}");
    assert!(took + Duration::from_secs(3) < HANDSHAKE, "{took:?}");
    // Nothing was asked once that time had run out.
    let asked = std::fs::read_to_string(&asked).unwrap().lines().count();
    assert!(asked <= 2, "asked {asked} times");
    endpoint.close().await;
}

/// An older client reads the line as a broken handshake, and says that instead of the hint
/// it gives for a session that just ends.
#[cfg(unix)]
#[tokio::test]
async fn a_client_that_did_not_ask_is_told_nothing_it_cannot_read() {
    for (saved, cli, says) in [
        (
            Some(uuid::Uuid::new_v4().to_string()),
            true,
            "identity changed",
        ),
        (
            Some("not an identity".to_owned()),
            true,
            "invalid saved host identity",
        ),
        (None, false, "cannot start the Somtus CLI"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let route = route();
        if let Some(saved) = saved {
            std::fs::write(tmp.path().join("host-identity"), saved).unwrap();
        }
        let cli = if cli {
            cli_with_local(tmp.path(), HOSTS_LOCAL, LOCAL_FAILS)
        } else {
            tmp.path().join("no-such-cli")
        };
        let storage = Storage::at(tmp.path().into()).unwrap();
        let older = json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":route.host_id});
        let (endpoint, frame) = Endpoint::start(storage, cli, older, IDLE_LIMITS).await;
        assert_eq!(frame, None, "{says}");
        // What ends the session is what it was.
        let error = endpoint.task.await.unwrap().unwrap_err();
        assert!(format!("{error:#}").contains(says), "{error:#}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_dial_asks_the_host_to_say_why_it_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    let said = tmp.path().join("hello");
    failed_dial(&format!(
        "read -r hello; printf '%s' \"$hello\" > '{}'; exit 3",
        said.display()
    ))
    .await;
    let hello: Value = serde_json::from_slice(&std::fs::read(said).unwrap()).unwrap();
    assert_eq!(hello["type"], "hello", "{hello}");
    assert_eq!(hello["refused"], 1, "{hello}");
}

/// Each try of a route to a replaced host would otherwise start processes there.
#[cfg(unix)]
#[tokio::test]
async fn a_saved_identity_that_differs_is_refused_without_running_the_cli() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    std::fs::write(
        tmp.path().join("host-identity"),
        uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    let cli = cli_with_local(tmp.path(), HOSTS_LOCAL, LOCAL_FAILS);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let (endpoint, frame) = Endpoint::start(storage, cli, direct_hello(&route), IDLE_LIMITS).await;
    assert_eq!(frame, refused("identity_changed"));
    let error = endpoint.task.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("identity changed"), "{error:#}");
    assert_eq!(calls_of(tmp.path()), "");
}

#[cfg(unix)]
#[tokio::test]
async fn an_older_cli_that_starts_is_no_reason_to_refuse_a_saved_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    std::fs::write(tmp.path().join("host-identity"), &route.host_id).unwrap();
    let cli = cli_with_local(tmp.path(), "[]", LOCAL_FAILS);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let (endpoint, ready) = Endpoint::start(storage, cli, direct_hello(&route), IDLE_LIMITS).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    endpoint.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn ready_says_only_when_the_home_looks_new() {
    let id = uuid::Uuid::new_v4().to_string();
    for (name, new_home) in [
        (None, Some(json!(true))),
        (Some("state.json"), None),
        (Some("host-identity"), None),
        // A host that never had a project can still hold terminals and chats.
        (Some("sessions.json"), None),
        (Some("chats"), None),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let mut route = route();
        route.host_id = id.clone();
        match name {
            Some("chats") => std::fs::create_dir(tmp.path().join("chats")).unwrap(),
            Some("host-identity") => {
                std::fs::write(tmp.path().join("host-identity"), &id).unwrap();
            }
            Some(name) => std::fs::write(tmp.path().join(name), "{}").unwrap(),
            None => {}
        }
        let local = format!("print(json.dumps({{'v':1,'value':{{'hostId':'{id}'}}}}))");
        let cli = cli_with_local(tmp.path(), HOSTS_LOCAL, &local);
        let storage = Storage::at(tmp.path().into()).unwrap();
        let (endpoint, ready) =
            Endpoint::start(storage, cli, direct_hello(&route), IDLE_LIMITS).await;
        let ready = ready.expect("ready");
        assert_eq!(ready["type"], "ready");
        assert_eq!(ready.get("new_home").cloned(), new_home, "{name:?}");
        endpoint.close().await;
    }
}

/// A session that looks at its identity file far more often than every few seconds.
#[cfg(unix)]
const LOOKS_OFTEN: IdleLimits = IdleLimits {
    renew: Duration::from_millis(50),
    ..IDLE_LIMITS
};

#[cfg(unix)]
#[tokio::test]
async fn a_replaced_home_ends_a_running_session() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let path = tmp.path().join("host-identity");
    std::fs::write(&path, &route.host_id).unwrap();
    let cli = cli(tmp.path(), &route.host_id);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let (mut endpoint, ready) =
        Endpoint::start(storage, cli, direct_hello(&route), LOOKS_OFTEN).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    std::fs::write(&path, uuid::Uuid::new_v4().to_string()).unwrap();
    let error = timeout(Duration::from_secs(10), &mut endpoint.task)
        .await
        .expect("the session notices within a tick")
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("identity changed"), "{error:#}");
}

/// A file that cannot be read says nothing about which installation this is, and
/// healthy terminals must not end for it.
#[cfg(unix)]
#[tokio::test]
async fn a_damaged_or_missing_identity_file_does_not_end_a_running_session() {
    async fn stays_up(damage: fn(&std::path::Path)) {
        let tmp = tempfile::tempdir().unwrap();
        let route = route();
        let path = tmp.path().join("host-identity");
        std::fs::write(&path, &route.host_id).unwrap();
        let cli = cli(tmp.path(), &route.host_id);
        let storage = Storage::at(tmp.path().into()).unwrap();
        let (mut endpoint, ready) =
            Endpoint::start(storage, cli, direct_hello(&route), LOOKS_OFTEN).await;
        assert_eq!(ready.expect("ready")["type"], "ready");
        damage(&path);
        // Many of its looks at the file.
        tokio::time::sleep(Duration::from_millis(500)).await;
        write_json(&mut endpoint.writer, &json!({"v":1,"type":"ping"}))
            .await
            .unwrap();
        let pong = timeout(
            Duration::from_secs(3),
            read_json(&mut endpoint.reader, &mut endpoint.partial, MAX_LINE),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        assert_eq!(pong["type"], "pong");
        endpoint.close().await;
    }
    tokio::join!(
        stays_up(|path| std::fs::write(path, "garbage").unwrap()),
        stays_up(|path| std::fs::remove_file(path).unwrap()),
        stays_up(|path| {
            use std::os::unix::ffi::OsStrExt;
            std::fs::remove_file(path).unwrap();
            let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        }),
    );
}

/// A lock wait leaves the question something, the questions of `Rpc::phone_features` have a
/// moment after it, and a CLI that never answers leaves `ssh` seconds of the client's limit
/// to connect and log in.
#[test]
fn the_handshakes_waits_fit_inside_the_clients_limit() {
    assert!(FORWARDED_LOCK_WAIT < CLI_QUESTION_WAIT);
    assert!(CLI_QUESTION_WAIT < FEATURES_BY);
    assert!(FEATURES_BY + Duration::from_secs(4) <= HANDSHAKE);
}

#[test]
fn a_forwarded_session_gives_up_sooner_than_a_direct_one_but_not_within_a_few_pings() {
    assert_eq!(IDLE_LIMITS.direct, Duration::from_secs(60));
    assert_eq!(IDLE_LIMITS.forwarded, Duration::from_secs(25));
    assert!(IDLE_LIMITS.forwarded > LinkTiming::default().ping * 2);
}

#[cfg(unix)]
#[tokio::test]
async fn a_silent_forwarded_session_ends_at_its_limit_and_frees_its_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let cli = cli(tmp.path(), &route.host_id);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let idle = IdleLimits {
        direct: Duration::from_secs(60),
        forwarded: Duration::from_millis(500),
        ..IDLE_LIMITS
    };
    let scope = uuid::Uuid::new_v4().to_string();
    let hello = json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":route.host_id,"session_id":scope});
    let (mut forwarded, ready) = Endpoint::start(storage.clone(), cli.clone(), hello, idle).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    let (mut direct, ready) =
        Endpoint::start(storage.clone(), cli, direct_hello(&route), idle).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    let error = timeout(Duration::from_secs(10), &mut forwarded.task)
        .await
        .expect("the silent session ends")
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("stopped answering"), "{error:#}");
    drop(
        storage
            .lock_wait(
                &format!("ssh-forwarded-{}.lock", route.client_id),
                Duration::ZERO,
            )
            .unwrap(),
    );
    // The same silence is nothing to a direct session.
    write_json(&mut direct.writer, &json!({"v":1,"type":"ping"}))
        .await
        .unwrap();
    let pong = read_json(&mut direct.reader, &mut direct.partial, MAX_LINE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pong["type"], "pong");
    direct.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_request_arriving_slowly_counts_as_the_client_answering() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let cli = cli(tmp.path(), &route.host_id);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let idle = IdleLimits {
        direct: Duration::from_millis(600),
        forwarded: Duration::from_millis(600),
        ..IDLE_LIMITS
    };
    let (mut endpoint, ready) = Endpoint::start(storage, cli, direct_hello(&route), idle).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    for byte in br#"{"v":1,"type":"ping"}"#.iter() {
        endpoint.writer.write_all(&[*byte]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    endpoint.writer.write_all(b"\n").await.unwrap();
    let pong = timeout(
        Duration::from_secs(3),
        read_json(&mut endpoint.reader, &mut endpoint.partial, MAX_LINE),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(pong["type"], "pong");
    // But a frame that stops growing is silence like any other.
    endpoint.writer.write_all(b"{\"v\":").await.unwrap();
    let error = timeout(Duration::from_secs(10), &mut endpoint.task)
        .await
        .expect("the stalled session ends")
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("stopped answering"), "{error:#}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_reconnect_waits_out_a_session_that_is_finishing() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let cli = cli(tmp.path(), &route.host_id);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let hello = || json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":route.host_id,"session_id":uuid::Uuid::new_v4().to_string()});
    let (first, ready) = Endpoint::start(storage.clone(), cli.clone(), hello(), IDLE_LIMITS).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    // Longer than the wait used to be, well inside the one it is now.
    let closing = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(2500)).await;
        first.close().await;
    });
    let (second, ready) = Endpoint::start(storage, cli, hello(), IDLE_LIMITS).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    closing.await.unwrap();
    second.close().await;
}

/// A CLI with one live shell, whose `shell resize` fails the way the real one does with
/// what the file `resize-says` holds. The file `gone` takes the shell out of its list, and
/// `long-reply` makes `native.invoke` answer with far more than the pipe holds.
#[cfg(unix)]
fn lease_cli(tmp: &std::path::Path, route: &Route, shell: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = tmp.join("lease-cli");
    let body = r#"#!/bin/sh
D='@DIR@'
printf '%s\n' "$*" >> "$D/calls"
case "$1 $2" in
capabilities*) echo '{"v":1,"native":{"v":1,"methods":["Hosts.local"]}}';;
'native invoke') if [ -e "$D/long-reply" ]; then printf '{"v":1,"value":{"hostId":"@HOST@","pad":"'; head -c 1000000 /dev/zero | tr '\0' x; printf '"}}\n'; else echo '{"v":1,"value":{"hostId":"@HOST@"}}'; fi;;
'shell list') if [ -e "$D/gone" ]; then echo '[]'; else printf '[{"id":"@SHELL@","alive":true,"mode":"terminal"}]'; fi;;
'orchestrator list'|'project list') echo '[]';;
'shell resize') if [ -e "$D/resize-says" ]; then cat "$D/resize-says" >&2; exit 2; fi;;
*) echo '{}';;
esac
"#
    .replace("@DIR@", &tmp.to_string_lossy())
    .replace("@HOST@", &route.host_id)
    .replace("@SHELL@", shell);
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    script
}
#[cfg(unix)]
fn renewals(tmp: &std::path::Path) -> usize {
    calls_of(tmp)
        .lines()
        .filter(|line| line.starts_with("shell resize "))
        .count()
}
/// Every renewal starts a CLI, which a loaded machine can be slow to start.
#[cfg(unix)]
async fn until_renewed_more_than(tmp: &std::path::Path, count: usize) {
    timeout(Duration::from_secs(20), async {
        while renewals(tmp) <= count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("renewed");
}
/// Until none has shown for many ticks: a renewal shows only once its CLI has started.
#[cfg(unix)]
async fn until_no_more_renewals(tmp: &std::path::Path) {
    let mut seen = renewals(tmp);
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let now = renewals(tmp);
        if now == seen {
            return;
        }
        seen = now;
    }
    panic!("renewals never stopped");
}

/// Returns once the reply has begun to arrive: from then on the session is writing it, and
/// it cannot finish until this end reads.
#[cfg(unix)]
async fn ask_for_a_long_reply(endpoint: &mut Endpoint, tmp: &std::path::Path) {
    std::fs::write(tmp.join("long-reply"), "").unwrap();
    write_json(&mut endpoint.writer, &json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":"native.invoke","params":{"module":"Hosts","method":"local","args":{}}})).await.unwrap();
    timeout(Duration::from_secs(10), endpoint.reader.fill_buf())
        .await
        .expect("the reply begins")
        .unwrap();
}
#[cfg(unix)]
async fn the_long_reply(endpoint: &mut Endpoint) -> Value {
    timeout(
        Duration::from_secs(10),
        read_json(&mut endpoint.reader, &mut endpoint.partial, MAX_LINE),
    )
    .await
    .expect("the reply arrives")
    .unwrap()
    .expect("the reply arrives")
}

/// Neither end reads while it writes, so a client's pings wait in the pipe behind a long
/// reply, and on real stdin a read that starts now answers only after the deadline is looked at.
#[cfg(unix)]
#[tokio::test]
async fn time_spent_writing_a_reply_is_not_the_client_being_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let cli = lease_cli(tmp.path(), &route, &uuid::Uuid::new_v4().to_string());
    let storage = Storage::at(tmp.path().into()).unwrap();
    let silence = Duration::from_secs(4);
    let idle = IdleLimits {
        direct: silence,
        forwarded: silence,
        ..IDLE_LIMITS
    };
    let (mut endpoint, ready) = Endpoint::start(storage, cli, direct_hello(&route), idle).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    ask_for_a_long_reply(&mut endpoint, tmp.path()).await;
    tokio::time::sleep(silence + Duration::from_millis(500)).await;
    let reply = the_long_reply(&mut endpoint).await;
    assert_eq!(reply["ok"], true, "the whole reply came");
    // The write took longer than the limit, and the session is still there to be asked.
    endpoint.ping().await;
    endpoint.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_client_that_takes_nothing_still_ends_the_session() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let cli = lease_cli(tmp.path(), &route, &uuid::Uuid::new_v4().to_string());
    let storage = Storage::at(tmp.path().into()).unwrap();
    let (mut endpoint, ready) =
        Endpoint::start(storage, cli, direct_hello(&route), IDLE_LIMITS).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    ask_for_a_long_reply(&mut endpoint, tmp.path()).await;
    let error = timeout(WRITE_WAIT * 3, &mut endpoint.task)
        .await
        .expect("the write gives up")
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error:#}");
}

/// The size a phone holds must outlast a long reply on the same connection: renewals stop
/// for no client's reason otherwise, and the lease lapses under a live terminal.
#[cfg(unix)]
#[tokio::test]
async fn a_long_write_does_not_stop_the_lease_being_renewed() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let shell = uuid::Uuid::new_v4().to_string();
    let cli = lease_cli(tmp.path(), &route, &shell);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let limits = IdleLimits {
        renew: Duration::from_millis(100),
        ..IDLE_LIMITS
    };
    let (mut endpoint, ready) = Endpoint::start(storage, cli, direct_hello(&route), limits).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    let resized = endpoint
        .call(
            "shell.resize",
            json!({"shell_id":shell,"columns":80,"rows":24}),
        )
        .await;
    assert_eq!(resized["ok"], true, "{resized}");
    ask_for_a_long_reply(&mut endpoint, tmp.path()).await;
    // One may have been under way when the write began.
    let before = renewals(tmp.path());
    until_renewed_more_than(tmp.path(), before + 1).await;
    assert_eq!(the_long_reply(&mut endpoint).await["ok"], true);
    endpoint.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_forwarded_session_renews_only_while_requests_come_and_a_direct_one_goes_on() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let shell = uuid::Uuid::new_v4().to_string();
    let cli = lease_cli(tmp.path(), &route, &shell);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let limits = IdleLimits {
        renew: Duration::from_millis(100),
        renewal_window: Duration::from_millis(500),
        ..IDLE_LIMITS
    };
    let hello = json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":route.host_id,"session_id":uuid::Uuid::new_v4().to_string()});
    let (mut forwarded, ready) = Endpoint::start(storage.clone(), cli.clone(), hello, limits).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    let size = json!({"shell_id":shell,"columns":80,"rows":24});
    let resized = forwarded.call("shell.resize", size.clone()).await;
    assert_eq!(resized["ok"], true, "{resized}");
    // Renewals go on for as long as there are requests, past the window.
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(800) || renewals(tmp.path()) < 5 {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "renewals stopped while requests came: {}",
            renewals(tmp.path())
        );
        assert_eq!(forwarded.call("projects.list", json!({})).await["ok"], true);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Pings say the link is alive, not that anyone is looking at the terminal. A renewal
    // shows only once its CLI has started, which a loaded machine can take seconds to do,
    // so the one under way as the window closed may show late. They go one at a time, so
    // no other can.
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1500) {
        forwarded.ping().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let stopped = renewals(tmp.path());
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1500) {
        forwarded.ping().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let late = renewals(tmp.path()) - stopped;
    assert!(late <= 1, "{late} renewals on pings alone");
    // The next request of any kind picks it up again, with no new resize.
    let quiet = renewals(tmp.path());
    assert_eq!(forwarded.call("projects.list", json!({})).await["ok"], true);
    until_renewed_more_than(tmp.path(), quiet).await;
    forwarded.close().await;

    // A direct session has no phone to go quiet: its client's pings are enough.
    let mut other = route.clone();
    other.client_id = uuid::Uuid::new_v4().to_string();
    let (mut direct, ready) = Endpoint::start(storage, cli, direct_hello(&other), limits).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    let resized = direct.call("shell.resize", size).await;
    assert_eq!(resized["ok"], true, "{resized}");
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1000) {
        direct.ping().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let before = renewals(tmp.path());
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(300) || renewals(tmp.path()) <= before {
        direct.ping().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "renewals stopped"
        );
    }
    direct.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_shell_that_ended_ends_the_lease_and_not_the_session() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let shell = uuid::Uuid::new_v4().to_string();
    let cli = lease_cli(tmp.path(), &route, &shell);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let limits = IdleLimits {
        renew: Duration::from_millis(100),
        ..IDLE_LIMITS
    };
    let (mut endpoint, ready) = Endpoint::start(storage, cli, direct_hello(&route), limits).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    let size = json!({"shell_id":shell,"columns":80,"rows":24});
    let resized = endpoint.call("shell.resize", size.clone()).await;
    assert_eq!(resized["ok"], true, "{resized}");
    until_renewed_more_than(tmp.path(), 2).await;
    // The shell ended: a typed `exit`, or closed on the desktop.
    std::fs::write(tmp.path().join("gone"), "").unwrap();
    std::fs::write(
        tmp.path().join("resize-says"),
        format!("somtus: shell {shell} has exited\n"),
    )
    .unwrap();
    until_no_more_renewals(tmp.path()).await;
    assert!(!endpoint.task.is_finished());
    assert_eq!(endpoint.call("projects.list", json!({})).await["ok"], true);
    // What the phone does when it finds out still works.
    let released = endpoint
        .call("shell.resize.clear", json!({"shell_id":shell}))
        .await;
    assert_eq!(released["result"]["status"], "cleared", "{released}");
    // The size can be asked for again, and someone else may hold it by then.
    std::fs::remove_file(tmp.path().join("gone")).unwrap();
    std::fs::remove_file(tmp.path().join("resize-says")).unwrap();
    let resized = endpoint.call("shell.resize", size).await;
    assert_eq!(resized["ok"], true, "{resized}");
    let renewed = renewals(tmp.path());
    until_renewed_more_than(tmp.path(), renewed).await;
    // Someone else holds it now. That lasts as long as they do, so it is asked again.
    std::fs::write(
        tmp.path().join("resize-says"),
        "somtus: viewport_busy: another authenticated connection owns this terminal size\n",
    )
    .unwrap();
    let renewed = renewals(tmp.path());
    until_renewed_more_than(tmp.path(), renewed + 2).await;
    assert_eq!(endpoint.call("projects.list", json!({})).await["ok"], true);
    endpoint.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_renewal_that_fails_for_no_stated_reason_goes_on_until_it_works() {
    let tmp = tempfile::tempdir().unwrap();
    let route = route();
    let shell = uuid::Uuid::new_v4().to_string();
    let cli = lease_cli(tmp.path(), &route, &shell);
    let storage = Storage::at(tmp.path().into()).unwrap();
    let limits = IdleLimits {
        renew: Duration::from_millis(100),
        ..IDLE_LIMITS
    };
    let (mut endpoint, ready) = Endpoint::start(storage, cli, direct_hello(&route), limits).await;
    assert_eq!(ready.expect("ready")["type"], "ready");
    let resized = endpoint
        .call(
            "shell.resize",
            json!({"shell_id":shell,"columns":80,"rows":24}),
        )
        .await;
    assert_eq!(resized["ok"], true, "{resized}");
    std::fs::write(
        tmp.path().join("resize-says"),
        "somtus: tmux did not answer\n",
    )
    .unwrap();
    // Its client believes it holds the size and will not ask again, so a pin does not end
    // for this. A later success takes a fresh lease.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let failing = renewals(tmp.path());
    until_renewed_more_than(tmp.path(), failing + 2).await;
    assert!(!endpoint.task.is_finished());
    assert_eq!(endpoint.call("projects.list", json!({})).await["ok"], true);
    std::fs::remove_file(tmp.path().join("resize-says")).unwrap();
    let failed = renewals(tmp.path());
    until_renewed_more_than(tmp.path(), failed + 1).await;
    endpoint.close().await;
}

/// `/bin/sh -c script` standing in for `ssh`.
#[cfg(unix)]
fn shell(script: &str) -> Process {
    let mut process = Process::new("/bin/sh");
    process
        .arg("-c")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    process
}
#[cfg(unix)]
async fn failed_dial(script: &str) -> anyhow::Error {
    let route = route();
    match dial_command(&route, None, shell(script)).await {
        Ok(_) => panic!("must not reach ready: {script}"),
        Err(error) => error,
    }
}

#[test]
fn a_status_table_of_exits_and_words_picks_the_failure() {
    use DialFailure::*;
    use Skipped::{Lines, Nothing, Unterminated};
    for (code, stderr, skipped, expected) in [
        (
            Some(255),
            "user@h: Permission denied (publickey).",
            Nothing,
            AuthFailed,
        ),
        (
            Some(255),
            "Received disconnect: Too many authentication failures",
            Nothing,
            AuthFailed,
        ),
        (Some(255), "Host key verification failed.", Nothing, HostKey),
        (
            Some(255),
            "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!",
            Nothing,
            HostKey,
        ),
        (
            Some(255),
            "No ED25519 host key is known for h and you have requested strict checking.",
            Nothing,
            HostKey,
        ),
        (
            Some(255),
            "ssh: Could not resolve hostname h: Name or service not known",
            Nothing,
            NameResolution,
        ),
        (
            Some(255),
            "ssh: Could not resolve hostname h: nodename nor servname provided, or not known",
            Nothing,
            NameResolution,
        ),
        (
            Some(255),
            "ssh: connect to host h port 22: Connection refused",
            Nothing,
            Unreachable,
        ),
        (
            Some(255),
            "ssh: connect to host h port 22: Operation timed out",
            Nothing,
            Unreachable,
        ),
        (
            Some(255),
            "ssh: connect to host h port 22: No route to host",
            Nothing,
            Unreachable,
        ),
        (
            Some(255),
            "ssh: connect to host h port 22: Network is unreachable",
            Nothing,
            Unreachable,
        ),
        (
            Some(255),
            "Connection closed by 10.0.0.1 port 22",
            Nothing,
            Unreachable,
        ),
        (
            Some(255),
            "kex_exchange_identification: read: Connection reset by peer",
            Nothing,
            Unreachable,
        ),
        (Some(255), "something else", Nothing, Closed),
        (Some(255), "", Nothing, Closed),
        (Some(126), "permission denied", Nothing, NotInstalled),
        (
            Some(127),
            "sh: somtus-remote: not found",
            Nothing,
            NotInstalled,
        ),
        // Older servers say these on stderr and exit with their own status.
        (
            Some(1),
            "Error: SSH host identity changed",
            Nothing,
            IdentityChanged,
        ),
        // An older server's sentence is read for the reason after it. Most reasons pass by
        // themselves, and a wrong path has a remedy of its own.
        (
            Some(1),
            "Error: installed Somtus must support Hosts.local for stable SSH identity: unsupported RPC method",
            Nothing,
            CliUnsupported,
        ),
        (
            Some(1),
            "Error: installed Somtus must support Hosts.local for stable SSH identity: unsupported native method",
            Nothing,
            CliUnsupported,
        ),
        (
            Some(1),
            "Error: installed Somtus must support Hosts.local for stable SSH identity: result exceeds encrypted response limit; reduce output lines",
            Nothing,
            CliUnsupported,
        ),
        (
            Some(1),
            "Error: installed Somtus must support Hosts.local for stable SSH identity: start configured Somtus CLI /typo",
            Nothing,
            CliMissing,
        ),
        (
            Some(1),
            "Error: installed Somtus must support Hosts.local for stable SSH identity: x",
            Nothing,
            Closed,
        ),
        (
            Some(1),
            "Error: installed Somtus must support Hosts.local for stable SSH identity: x",
            Lines,
            Closed,
        ),
        (
            Some(2),
            "error: unrecognized subcommand 'connect'",
            Nothing,
            NotInstalled,
        ),
        (
            Some(1),
            "Error: timed out waiting for another remote process using this storage",
            Nothing,
            SessionBusy,
        ),
        (
            Some(1),
            "Error: pass --somtus /absolute/path/to/somtus",
            Nothing,
            CliMissing,
        ),
        // Words of ssh's own count only when ssh itself is what failed.
        (Some(1), "Permission denied", Nothing, Closed),
        (Some(1), "", Nothing, Closed),
        (
            None,
            "Error: SSH host identity changed",
            Nothing,
            IdentityChanged,
        ),
        (None, "", Nothing, Closed),
        // Whole lines explain only a far side that gave up by itself, for want of anything
        // else. They do not bring ssh down or get a process killed.
        (Some(1), "", Lines, LoginOutput),
        (Some(255), "", Lines, Closed),
        (Some(137), "", Lines, Closed),
        (None, "", Lines, Closed),
        (
            Some(255),
            "client_loop: send disconnect: Broken pipe",
            Lines,
            Closed,
        ),
        // Text with no end of line can always be why: it swallows what comes next.
        (Some(1), "", Unterminated, LoginOutput),
        (Some(255), "", Unterminated, LoginOutput),
        (None, "", Unterminated, LoginOutput),
        // And neither explains what has words of its own.
        (Some(127), "not found", Lines, NotInstalled),
        (Some(127), "not found", Unterminated, NotInstalled),
        (
            Some(255),
            "user@h: Permission denied (publickey).",
            Lines,
            AuthFailed,
        ),
        (
            Some(255),
            "user@h: Permission denied (publickey).",
            Unterminated,
            AuthFailed,
        ),
        // A startup file's complaint on the way in is not a refused login.
        (Some(255), "bash: /x: Permission denied", Lines, Closed),
        (
            Some(1),
            "Error: SSH host identity changed",
            Lines,
            IdentityChanged,
        ),
    ] {
        assert_eq!(
            classify_exit(code, stderr, skipped),
            expected,
            "{code:?} {stderr} {skipped:?}"
        );
    }
}

#[test]
fn every_failure_has_a_code_fixed_words_an_action_and_a_gateway_code() {
    use DialFailure::*;
    let table = [
        (IdentityChanged, "identity_changed", true, "host_changed"),
        (
            IdentityUnavailable,
            "identity_unavailable",
            true,
            "host_unavailable",
        ),
        (
            CliUnsupported,
            "cli_unsupported",
            true,
            "host_not_installed",
        ),
        (CliMissing, "cli_missing", true, "host_not_installed"),
        (
            HomeUnavailable,
            "home_unavailable",
            true,
            "host_unavailable",
        ),
        (SessionBusy, "session_busy", false, "host_busy"),
        (AuthFailed, "auth_failed", true, "host_login_failed"),
        (HostKey, "host_key", true, "host_login_failed"),
        (NameResolution, "name_resolution", false, "host_unreachable"),
        (Unreachable, "unreachable", false, "host_unreachable"),
        (NotInstalled, "not_installed", true, "host_not_installed"),
        (LoginOutput, "login_output", true, "host_unavailable"),
        (HandshakeTimeout, "handshake_timeout", false, "host_timeout"),
        (SshNotStarted, "ssh_not_started", true, "host_unavailable"),
        (NoForwarding, "no_forwarding", true, "host_unsupported"),
        (Closed, "closed", false, "host_unavailable"),
    ];
    for (failure, code, needs_action, gateway) in table {
        assert_eq!(failure.code(), code);
        assert_eq!(failure.needs_action(), needs_action, "{code}");
        assert_eq!(failure.gateway_code(), gateway, "{code}");
        assert_eq!(failure.cause().code, code);
        assert_eq!(failure.cause().needs_action, needs_action);
        // Whole through `log_safe`, which cuts at 200 characters and is applied to
        // every reason.
        let message = failure.message();
        assert_eq!(crate::log_safe(message), message, "{code}");
    }
    for failure in [
        IdentityChanged,
        IdentityUnavailable,
        CliUnsupported,
        CliMissing,
        HomeUnavailable,
        SessionBusy,
    ] {
        assert_eq!(DialFailure::from_refused(failure.code()), failure);
    }
    // Only the frame's own codes: no other word gets a `refused` line to say more.
    assert_eq!(DialFailure::from_refused("auth_failed"), Closed);
    assert_eq!(DialFailure::from_refused("from the future"), Closed);
}

#[cfg(unix)]
#[tokio::test]
async fn a_dial_that_ends_says_why_in_fixed_words() {
    use DialFailure::*;
    for (script, expected) in [
        (
            "echo 'user@h: Permission denied (publickey).' >&2; exit 255",
            AuthFailed,
        ),
        (
            "echo 'Host key verification failed.' >&2; exit 255",
            HostKey,
        ),
        (
            "echo 'ssh: Could not resolve hostname h: nodename nor servname provided, or not known' >&2; exit 255",
            NameResolution,
        ),
        (
            "echo 'ssh: connect to host h port 22: Connection refused' >&2; exit 255",
            Unreachable,
        ),
        (
            "echo 'sh: somtus-remote: not found' >&2; exit 127",
            NotInstalled,
        ),
        (
            "echo 'Error: SSH host identity changed' >&2; exit 1",
            IdentityChanged,
        ),
        (
            "echo 'Error: installed Somtus must support Hosts.local for stable SSH identity: unsupported RPC method' >&2; exit 1",
            CliUnsupported,
        ),
        (
            "echo \"error: unrecognized subcommand 'connect'\" >&2; exit 2",
            NotInstalled,
        ),
        ("exit 3", Closed),
        // What a noisy login leaves when nothing else explains the end.
        ("echo Welcome; exit 1", LoginOutput),
        ("printf Welcome; exit 1", LoginOutput),
        // A wrong path stays a wrong path, however chatty the login.
        ("echo Welcome; exit 127", NotInstalled),
        // A refusal is the host's own word, ahead of everything else.
        (
            r#"echo '{"v":1,"type":"refused","code":"identity_changed"}'; exit 1"#,
            IdentityChanged,
        ),
        (
            r#"echo '{"v":1,"type":"refused","code":"identity_unavailable"}'; exit 1"#,
            IdentityUnavailable,
        ),
        (
            r#"echo '{"v":1,"type":"refused","code":"cli_unsupported"}'; exit 1"#,
            CliUnsupported,
        ),
        (
            r#"echo '{"v":1,"type":"refused","code":"cli_missing"}'; exit 1"#,
            CliMissing,
        ),
        (
            r#"echo '{"v":1,"type":"refused","code":"home_unavailable"}'; exit 1"#,
            HomeUnavailable,
        ),
        (
            r#"echo '{"v":1,"type":"refused","code":"session_busy"}'; exit 1"#,
            SessionBusy,
        ),
        (
            r#"echo Welcome; echo '{"v":1,"type":"refused","code":"session_busy"}'; exit 1"#,
            SessionBusy,
        ),
        (
            r#"echo '{"v":1,"type":"refused","code":"from the future"}'; exit 1"#,
            Closed,
        ),
    ] {
        let error = failed_dial(script).await;
        assert_eq!(dial_failure(&error), Some(expected), "{script}: {error:#}");
        assert_eq!(format!("{error:#}"), expected.message(), "{script}");
    }
}

/// Whole lines cost nothing and stop no frame from arriving, so a chatty login that works
/// every day must not turn a dial that failed for another reason into a person's problem.
#[cfg(unix)]
#[tokio::test]
async fn only_output_that_can_be_the_cause_is_called_login_output() {
    use DialFailure::*;
    let route = route();
    let ready = format!(r#"{{"v":1,"type":"ready","host_id":"{}"}}"#, route.host_id);
    let cut_off = ready[..ready.len() - 8].to_owned();
    let mut wrong = Vec::new();
    for (script, expected) in [
        // ssh's own failure, or a killed session, with only whole lines before it.
        ("echo Welcome; exit 255".to_owned(), Closed),
        (
            "echo Welcome; echo 'client_loop: send disconnect: Broken pipe' >&2; exit 255"
                .to_owned(),
            Closed,
        ),
        (
            "echo Welcome; echo 'Connection to h closed by remote host.' >&2; exit 255".to_owned(),
            Closed,
        ),
        (
            "echo Welcome; echo 'Timeout, server h not responding.' >&2; exit 255".to_owned(),
            Closed,
        ),
        ("echo Welcome; exit 137".to_owned(), Closed),
        ("echo Welcome; kill -9 $$".to_owned(), Closed),
        // A real `ready` that the link cut off is not text.
        (format!("printf '%s' '{cut_off}'; exit 255"), Closed),
        (
            format!("echo Welcome; printf '%s' '{cut_off}'; exit 255"),
            Closed,
        ),
        // What it can explain: a far side that gave up by itself, and text with no end of line.
        ("echo Welcome; exit 1".to_owned(), LoginOutput),
        ("printf Welcome; exit 1".to_owned(), LoginOutput),
        ("printf Welcome; exit 255".to_owned(), LoginOutput),
        (
            format!("printf 'Welcome%s' '{cut_off}'; exit 255"),
            LoginOutput,
        ),
        (
            format!("printf Welcome; echo '{ready}'; exit 255"),
            LoginOutput,
        ),
        // A failure with words of its own keeps them.
        (
            "echo Welcome; echo 'user@h: Permission denied (publickey).' >&2; exit 255".to_owned(),
            AuthFailed,
        ),
    ] {
        let error = failed_dial(&script).await;
        if dial_failure(&error) != Some(expected) {
            wrong.push(format!("{script}: {error:#}"));
        }
    }
    // The handshake's timer, side by side because each row waits for it. A row that has to
    // print is given time for a loaded machine to start a shell, and outlives the timer.
    let mut timed = JoinSet::new();
    for (script, expected) in [
        ("sleep 30".to_owned(), HandshakeTimeout),
        ("echo Welcome; sleep 30".to_owned(), HandshakeTimeout),
        (
            "echo Welcome; echo; echo 42; echo '[1,2]'; sleep 30".to_owned(),
            HandshakeTimeout,
        ),
        (
            "while :; do echo Welcome; sleep 0.1; done".to_owned(),
            HandshakeTimeout,
        ),
        (
            format!("printf '%s' '{cut_off}'; sleep 30"),
            HandshakeTimeout,
        ),
        (
            format!("echo Welcome; printf '%s' '{cut_off}'; sleep 30"),
            HandshakeTimeout,
        ),
        (
            r#"printf '{"features":{"chat":tr'; sleep 30"#.to_owned(),
            HandshakeTimeout,
        ),
        ("printf Welcome; sleep 30".to_owned(), LoginOutput),
        ("printf '{not a frame'; sleep 30".to_owned(), LoginOutput),
        (
            format!("printf 'Welcome%s' '{cut_off}'; sleep 30"),
            LoginOutput,
        ),
        // The banner had no end of line, so the frame became part of its line.
        (
            format!("printf Welcome; echo '{ready}'; sleep 30"),
            LoginOutput,
        ),
    ] {
        let route = route.clone();
        timed.spawn(async move {
            let got = match dial_within(&route, None, shell(&script), None, Duration::from_secs(5))
                .await
            {
                Ok(_) => panic!("must not reach ready: {script}"),
                Err(error) => dial_failure(&error),
            };
            (got != Some(expected)).then(|| format!("{script}: {got:?}"))
        });
    }
    while let Some(row) = timed.join_next().await {
        wrong.extend(row.unwrap());
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// An older server gives one sentence for every failure of `Hosts.local`, with the reason
/// after it. Most of those pass by themselves, and the path has its own remedy.
#[cfg(unix)]
#[tokio::test]
async fn an_older_hosts_sentence_is_read_for_the_reason_it_gives() {
    use DialFailure::*;
    let says = |reason: &str| {
        format!(
            "echo 'Error: installed Somtus must support Hosts.local for stable SSH identity: {reason}' >&2; exit 1"
        )
    };
    let mut wrong = Vec::new();
    for (script, expected) in [
        (says("start configured Somtus CLI /typo/somtus"), CliMissing),
        (says("unsupported RPC method"), CliUnsupported),
        (says("unsupported native method"), CliUnsupported),
        (
            says("result exceeds encrypted response limit; reduce output lines"),
            CliUnsupported,
        ),
        (
            says("CLI output exceeds the encrypted response limit; reduce output lines"),
            CliUnsupported,
        ),
        (says("Too large to show on the phone"), CliUnsupported),
        (says("CLI response exceeded limit"), CliUnsupported),
        (
            says("Somtus CLI failed: somtus: tmux did not answer"),
            Closed,
        ),
        (says("Somtus CLI timeout"), Closed),
        (says("invalid response"), Closed),
        // It said what happened, so a chatty login is not the reason.
        (
            format!("echo Welcome; {}", says("Somtus CLI timeout")),
            Closed,
        ),
    ] {
        let error = failed_dial(&script).await;
        if dial_failure(&error) != Some(expected) || format!("{error:#}") != expected.message() {
            wrong.push(format!("{script}: {:?}", dial_failure(&error)));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert!(!Closed.needs_action(), "the normal pace of retries");
}

#[cfg(unix)]
#[tokio::test]
async fn what_ssh_printed_stays_out_of_every_reason_and_is_bounded() {
    let error = failed_dial(
        "echo 'user@h: Permission denied (publickey), see /Users/secret/key' >&2; exit 255",
    )
    .await;
    let dial = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<DialError>())
        .unwrap();
    let shown = format!("{error} {error:#} {error:?} {dial} {dial:?}");
    assert!(!shown.contains("secret"), "{shown}");
    let outcome = dial_outcome(&error);
    assert_eq!(outcome.reason, DialFailure::AuthFailed.message());
    assert_eq!(outcome.failure, Some(DialFailure::AuthFailed));
    assert_eq!(
        dial_output(&error),
        ["user@h: Permission denied (publickey), see /Users/secret/key"]
    );

    // An error that is not a dial's keeps today's wording and knows no cause.
    let plain = dial_outcome(&anyhow::anyhow!("invalid SSH ready handshake"));
    assert_eq!(plain.reason, "SSH: invalid SSH ready handshake");
    assert_eq!(plain.failure, None);
    assert!(dial_output(&anyhow::anyhow!("nothing here")).is_empty());

    // Far more than the tail holds: the end of it, as lines safe to print.
    let error = failed_dial(
        "i=0; while [ $i -lt 3000 ]; do printf 'line \\033[31m%s\\n' $i; i=$((i+1)); done >&2; exit 255",
    )
    .await;
    let dial = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<DialError>())
        .unwrap();
    assert!(dial.stderr.len() <= STDERR_TAIL, "{}", dial.stderr.len());
    let output = dial_output(&error);
    assert_eq!(output.len(), STDERR_LINES);
    assert!(
        output
            .iter()
            .all(|line| !line.chars().any(char::is_control))
    );
    assert!(output.last().unwrap().ends_with("2999"), "{output:?}");
}

/// Skipped lines cost nothing: only the frame that follows them is judged.
#[cfg(unix)]
#[tokio::test]
async fn login_noise_before_ready_is_skipped_and_ready_is_still_checked() {
    let route = route();
    let frames = format!(
        r#"echo Welcome; echo; echo 42; echo '[1,2]'; echo '{{"motd":"hi"}}'; echo '{{"v":1}}'; echo '{{"v":1,"type":"ready","host_id":"{}"}}'; sleep 5"#,
        route.host_id
    );
    let (_dialed, ready) = match dial_command(&route, Some(&route.host_id), shell(&frames)).await {
        Ok(dialed) => dialed,
        Err(error) => panic!("{error:#}"),
    };
    assert_eq!(ready["host_id"], route.host_id.as_str());
    // A pin is not something noise can talk past.
    let other = uuid::Uuid::new_v4().to_string();
    let error = match dial_command(&route, Some(&other), shell(&frames)).await {
        Ok(_) => panic!("the pin must hold"),
        Err(error) => error,
    };
    assert_eq!(dial_failure(&error), Some(DialFailure::IdentityChanged));
}

#[cfg(unix)]
#[tokio::test]
async fn a_handshake_that_never_finishes_is_told_from_one_that_printed_text() {
    let route = route();
    // A case that has to print is given time for a loaded machine to start a shell; one
    // that needs no output, or takes either answer, is not. They run side by side.
    let (prints, quiet) = (Duration::from_secs(5), Duration::from_secs(1));
    // `ready` glued to a banner with no newline can never be read.
    let glued = format!(
        r#"printf Welcome; echo '{{"v":1,"type":"ready","host_id":"{}"}}'; sleep 30"#,
        route.host_id
    );
    let cases = [
        (glued, prints),
        // However much of it there is, within the bounds of a handshake.
        (
            "head -c 3000000 /dev/zero | tr '\\0' x; sleep 30".to_owned(),
            prints,
        ),
        ("sleep 30".to_owned(), quiet),
        // Lines that never stop outlast nothing either.
        ("while :; do echo Welcome; done".to_owned(), quiet),
        // What was printed before the hang is there for the person who asked: this is
        // how a login that waits for a browser says where to go.
        (
            "echo 'visit https://login.example/x' >&2; sleep 30".to_owned(),
            prints,
        ),
    ];
    let mut dials = JoinSet::new();
    for (case, (script, limit)) in cases.iter().cloned().enumerate() {
        let route = route.clone();
        dials.spawn(async move {
            let started = Instant::now();
            match dial_within(&route, None, shell(&script), None, limit).await {
                Ok(_) => panic!("must not reach ready: {script}"),
                Err(error) => (case, error, started.elapsed()),
            }
        });
    }
    let mut ended = Vec::new();
    while let Some(dialed) = dials.join_next().await {
        ended.push(dialed.unwrap());
    }
    ended.sort_by_key(|(case, ..)| *case);
    for (case, _, took) in &ended {
        assert!(
            *took < Duration::from_secs(15),
            "{}: {took:?}",
            cases[*case].0
        );
    }
    let error = |case: usize| &ended[case].1;
    let failure = |case: usize| dial_failure(error(case));
    assert_eq!(failure(0), Some(DialFailure::LoginOutput), "{:#}", error(0));
    assert_eq!(failure(1), Some(DialFailure::LoginOutput), "{:#}", error(1));
    assert_eq!(
        failure(2),
        Some(DialFailure::HandshakeTimeout),
        "{:#}",
        error(2)
    );
    // Where the timer falls in a line decides which it is called: only a whole line is
    // known to be harmless.
    assert!(
        matches!(
            failure(3),
            Some(DialFailure::LoginOutput | DialFailure::HandshakeTimeout)
        ),
        "{:#}",
        error(3)
    );
    assert_eq!(dial_output(error(4)), ["visit https://login.example/x"]);
}

/// `shell(script)` for a far side that will not take the hello: by the time it starts it
/// has printed `said` and closed its end of stdin, so writing the hello fails every time.
/// A script that closes stdin itself does so only after the hello is in the pipe.
#[cfg(unix)]
fn shell_that_said(said: &str, script: &str) -> Process {
    let mut process = shell(script);
    let said = said.as_bytes().to_vec();
    // SAFETY: between fork and exec this calls only `write` and `close`, which are
    // async-signal-safe, on bytes made before the fork. `spawn` returns after the exec.
    unsafe {
        process.pre_exec(move || {
            if !said.is_empty() {
                libc::write(1, said.as_ptr().cast(), said.len());
            }
            libc::close(0);
            Ok(())
        });
    }
    process
}

#[cfg(unix)]
#[tokio::test]
async fn a_far_side_that_will_not_take_the_hello_can_still_say_why() {
    let route = route();
    let dial = async |process: Process| match dial_command(&route, None, process).await {
        Ok(_) => panic!("must not reach ready"),
        Err(error) => error,
    };
    let refused = "{\"v\":1,\"type\":\"refused\",\"code\":\"cli_missing\"}\n";
    let error = dial(shell_that_said(refused, "sleep 1; exit 1")).await;
    assert_eq!(
        dial_failure(&error),
        Some(DialFailure::CliMissing),
        "{error:#}"
    );
    // A far side that cannot take the hello has said all it will, so one that said
    // nothing is not given a whole handshake.
    let started = Instant::now();
    let error = dial(shell_that_said("", "sleep 20")).await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(dial_failure(&error), Some(DialFailure::Closed), "{error:#}");
}

/// A master connection kept by `ControlPersist` holds ssh's stderr open after ssh
/// has exited; the dial must not wait for it.
#[cfg(unix)]
#[tokio::test]
async fn stderr_held_open_by_another_process_does_not_hold_up_the_dial() {
    let started = Instant::now();
    let error = failed_dial(
        "sleep 20 >/dev/null & echo 'user@h: Permission denied (publickey).' >&2; exit 255",
    )
    .await;
    assert_eq!(
        dial_failure(&error),
        Some(DialFailure::AuthFailed),
        "{error:#}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[cfg(unix)]
#[tokio::test]
async fn ssh_that_cannot_start_is_its_own_failure() {
    let route = route();
    let mut missing = Process::new("/nonexistent/ssh");
    missing
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let error = match dial_command(&route, None, missing).await {
        Ok(_) => panic!("must not start"),
        Err(error) => error,
    };
    assert_eq!(dial_failure(&error), Some(DialFailure::SshNotStarted));
    assert_eq!(format!("{error:#}"), DialFailure::SshNotStarted.message());
}

#[test]
fn a_host_without_forwarded_sessions_is_told_apart() {
    let session = uuid::Uuid::new_v4().to_string();
    let good = json!({"session_id":session,"features":{"forwarded_sessions":1}});
    validate_forwarded(&good, &session).unwrap();
    for ready in [
        json!({"session_id":session,"features":{}}),
        json!({"features":{"forwarded_sessions":1}}),
    ] {
        let error = validate_forwarded(&ready, &session).unwrap_err();
        assert_eq!(dial_failure(&error), Some(DialFailure::NoForwarding));
    }
}

#[test]
fn a_phones_link_notices_a_silent_host_sooner_and_a_desktops_keeps_its_timing() {
    let (phone, desktop) = (gateway_timing(), LinkTiming::default());
    assert_eq!(phone.ping, Duration::from_secs(5));
    assert_eq!(phone.silence, Duration::from_secs(20));
    assert_eq!(desktop.ping, Duration::from_secs(10));
    assert_eq!(desktop.silence, Duration::from_secs(35));
    // The target does not mistake a gateway that has nothing to ask for a dead one.
    assert!(phone.ping * 2 < IDLE_LIMITS.forwarded);
}

/// Answers the handshake, then every request with a frame sent 200 bytes at a time.
/// It never answers a ping, so only those bytes can tell the link it is alive.
#[cfg(unix)]
fn slow_host(route: &Route, pieces: &str) -> Process {
    let mut process = Process::new("python3");
    process
        .arg("-u")
        .arg("-c")
        .arg(format!(
            r#"
import sys,json,time
hello=json.loads(sys.stdin.readline())
print(json.dumps({{'v':1,'type':'ready','host_id':{host:?}}}),flush=True)
for line in sys.stdin:
 frame=json.loads(line)
 if frame['type']=='ping': continue
 text=json.dumps({{'v':1,'type':'response','id':frame['id'],'ok':True,'result':{{'x':'y'*1000}}}})
 {pieces}
"#,
            host = route.host_id,
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    process
}
/// One request on a link that gives the host `silence`.
#[cfg(unix)]
async fn request_of(host: Process, route: &Route, silence: Duration) -> Result<Reply, CallError> {
    let (dialed, ready) = match dial_command(route, Some(&route.host_id), host).await {
        Ok(dialed) => dialed,
        Err(error) => panic!("{error:#}"),
    };
    let timing = LinkTiming {
        ping: Duration::from_millis(100),
        silence,
        ..LinkTiming::default()
    };
    let (state, _) = watch::channel(LinkState::Online { rtt_ms: None });
    let (conn, mut current) = watch::channel(None);
    let task =
        tokio::spawn(async move { connected(dialed, ready, timing, &state, &conn, &mut 0).await });
    current.changed().await.unwrap();
    let connection = current.borrow().clone().unwrap();
    let (reply, rx) = oneshot::channel();
    connection
        .tx
        .send(Command::Request {
            id: uuid::Uuid::new_v4().to_string(),
            method: "read".into(),
            params: json!({}),
            reply,
        })
        .await
        .unwrap();
    let answer = timeout(Duration::from_secs(10), rx).await.unwrap().unwrap();
    drop(connection);
    task.abort();
    answer
}

#[cfg(unix)]
#[tokio::test]
async fn the_end_of_a_session_tells_what_was_written_from_what_was_still_queued() {
    let route = route();
    // Reads what it is sent and answers nothing, so the first request stays in flight.
    let host = slow_host(&route, "pass");
    let (dialed, ready) = match dial_command(&route, Some(&route.host_id), host).await {
        Ok(dialed) => dialed,
        Err(error) => panic!("{error:#}"),
    };
    let (state, _) = watch::channel(LinkState::Online { rtt_ms: None });
    let (conn, mut current) = watch::channel(None);
    let task = tokio::spawn(async move {
        connected(dialed, ready, LinkTiming::default(), &state, &conn, &mut 0).await
    });
    current.changed().await.unwrap();
    let connection = current.borrow().clone().unwrap();
    let mut answers = Vec::new();
    // The session ends at the first reset, and a second one must not hide what is behind it.
    for step in [
        "written",
        "reset",
        "queued",
        "reset",
        "queued behind the reset",
    ] {
        let command = if step == "reset" {
            Command::Reset("test".into())
        } else {
            let (reply, rx) = oneshot::channel();
            answers.push((step, rx));
            Command::Request {
                id: uuid::Uuid::new_v4().to_string(),
                method: "read".into(),
                params: json!({}),
                reply,
            }
        };
        connection.tx.send(command).await.unwrap();
    }
    for (step, rx) in answers {
        let answer = timeout(Duration::from_secs(10), rx)
            .await
            .unwrap()
            .unwrap_or_else(|_| panic!("{step}: the reply sender was dropped"));
        match (step, answer) {
            ("written", Err(CallError::Lost(reason))) => {
                assert!(reason.contains("test"), "{reason}");
            }
            ("queued" | "queued behind the reset", Err(CallError::Offline(reason))) => {
                assert!(reason.contains("test"), "{reason}");
            }
            (step, answer) => panic!("{step}: {:?}", answer.map(|_| "a reply")),
        }
    }
    task.await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_reply_arriving_slowly_counts_as_the_host_answering() {
    let route = route();
    // A dozen pieces, so the frame takes longer than the link's silence, and a piece can be
    // late by well over a second and still be in time.
    let host = slow_host(
        &route,
        "for i in range(0,len(text),100):\n  sys.stdout.write(text[i:i+100]);sys.stdout.flush();time.sleep(.3)\n print('',flush=True)",
    );
    let reply = request_of(host, &route, Duration::from_secs(2))
        .await
        .expect("the frame finished arriving");
    assert_eq!(reply.response["ok"], true);
}

#[cfg(unix)]
#[tokio::test]
async fn a_reply_that_stops_growing_is_silence_like_any_other() {
    let route = route();
    let host = slow_host(
        &route,
        "sys.stdout.write(text[:200]);sys.stdout.flush();time.sleep(30)",
    );
    let started = Instant::now();
    assert!(
        request_of(host, &route, Duration::from_secs(1))
            .await
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// A request far larger than the pipe, on a link that gives the host `silence`.
#[cfg(unix)]
async fn long_request_of(
    host: Process,
    route: &Route,
    silence: Duration,
) -> Result<Reply, CallError> {
    let (dialed, ready) = match dial_command(route, Some(&route.host_id), host).await {
        Ok(dialed) => dialed,
        Err(error) => panic!("{error:#}"),
    };
    let timing = LinkTiming {
        ping: Duration::from_millis(100),
        silence,
        ..LinkTiming::default()
    };
    let (state, _) = watch::channel(LinkState::Online { rtt_ms: None });
    let (conn, mut current) = watch::channel(None);
    let task =
        tokio::spawn(async move { connected(dialed, ready, timing, &state, &conn, &mut 0).await });
    current.changed().await.unwrap();
    let connection = current.borrow().clone().unwrap();
    let (reply, rx) = oneshot::channel();
    connection
        .tx
        .send(Command::Request {
            id: uuid::Uuid::new_v4().to_string(),
            method: "read".into(),
            params: json!({"pad":"x".repeat(120 * 1024)}),
            reply,
        })
        .await
        .unwrap();
    let answer = timeout(WRITE_WAIT * 3, rx)
        .await
        .expect("the request is settled")
        .unwrap();
    drop(connection);
    task.abort();
    answer
}

/// The host takes the request only after longer than the link gives a silent host, and
/// answers a moment later. All that time was this side's own write.
#[cfg(unix)]
#[tokio::test]
async fn time_spent_writing_a_request_is_not_the_host_being_silent() {
    let route = route();
    let mut host = Process::new("python3");
    host.arg("-u")
        .arg("-c")
        .arg(format!(
            r#"
import sys,json,time
hello=json.loads(sys.stdin.readline())
print(json.dumps({{'v':1,'type':'ready','host_id':{host:?}}}),flush=True)
time.sleep(5)
for line in sys.stdin:
 frame=json.loads(line)
 if frame['type']=='ping': continue
 time.sleep(.5)
 print(json.dumps({{'v':1,'type':'response','id':frame['id'],'ok':True,'result':{{}}}}),flush=True)
"#,
            host = route.host_id,
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let reply = long_request_of(host, &route, Duration::from_secs(4))
        .await
        .expect("the host answered");
    assert_eq!(reply.response["ok"], true);
}

#[cfg(unix)]
#[tokio::test]
async fn a_host_that_takes_nothing_still_ends_the_link() {
    let route = route();
    let host = shell(&format!(
        r#"read -r hello; echo '{{"v":1,"type":"ready","host_id":"{}"}}'; sleep 30"#,
        route.host_id
    ));
    match long_request_of(host, &route, Duration::from_secs(4)).await {
        Err(CallError::Lost(reason)) => assert!(reason.contains("timed out"), "{reason}"),
        answer => panic!("{:?}", answer.map(|_| "a reply")),
    }
}

#[tokio::test]
async fn a_large_frame_is_timed_by_its_progress_not_its_size() {
    let frame = vec![b'x'; 1024 * 1024];
    let wait = Duration::from_secs(1);
    // Far slower than `wait` allows for the whole frame, always moving.
    let (mut writer, mut reader) = duplex(8 * 1024);
    let drain = tokio::spawn(async move {
        let mut total = 0;
        let mut buffer = vec![0; 8 * 1024];
        loop {
            let read = reader.read(&mut buffer).await.unwrap();
            if read == 0 {
                return total;
            }
            total += read;
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    });
    let started = Instant::now();
    write_frame(&mut writer, &frame, wait).await.unwrap();
    assert!(started.elapsed() > wait, "{:?}", started.elapsed());
    drop(writer);
    assert_eq!(drain.await.unwrap(), frame.len());
    // A peer that takes nothing still ends it.
    let (mut writer, _reader) = duplex(8 * 1024);
    let started = Instant::now();
    let error = write_frame(&mut writer, &frame, Duration::from_millis(200))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error:#}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn a_frame_goes_out_in_flushed_chunks() {
    #[derive(Default)]
    struct Recording {
        writes: Vec<usize>,
        flushes: usize,
    }
    impl AsyncWrite for Recording {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.get_mut().writes.push(bytes.len());
            std::task::Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            self.get_mut().flushes += 1;
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
    let mut big = Recording::default();
    write_json(&mut big, &json!("x".repeat(200 * 1024)))
        .await
        .unwrap();
    assert!(
        big.writes.iter().all(|size| *size <= 64 * 1024),
        "{:?}",
        big.writes
    );
    assert_eq!(big.writes.iter().sum::<usize>(), 200 * 1024 + 3);
    assert_eq!(big.writes.len(), 4);
    assert_eq!(big.flushes, 4);
    let mut small = Recording::default();
    write_json(&mut small, &json!({"v":1,"type":"ping"}))
        .await
        .unwrap();
    assert_eq!(small.writes.len(), 1);
    assert_eq!(small.flushes, 1);
    assert!(
        write_json(&mut small, &json!("x".repeat(MAX_LINE)))
            .await
            .is_err(),
        "the limit on a frame is unchanged"
    );
}
