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
    write_json(&mut client,&json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":uuid::Uuid::new_v4().to_string()})).await.unwrap();
    write_json(&mut client,&json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":"native.invoke","params":{"module":"Store","method":"renameProject","args":{}}})).await.unwrap();
    let error = task.await.unwrap().unwrap_err();
    assert!(
        error.to_string().contains("identity changed"),
        "{error:#}; calls={:?}",
        std::fs::read_to_string(tmp.path().join("calls"))
    );
    let calls = std::fs::read_to_string(tmp.path().join("calls")).unwrap();
    assert!(!calls.contains("renameProject"));
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
    async fn open(storage: Storage, cli: PathBuf, route: &Route, scope: &str) -> Self {
        let (client, server) = duplex(16384);
        let (read, write) = tokio::io::split(server);
        let task = tokio::spawn(serve_io(storage, cli, read, write));
        let (reader, writer) = tokio::io::split(client);
        let mut endpoint = Self {
            reader: BufReader::new(reader),
            writer,
            partial: Vec::new(),
            task,
        };
        write_json(&mut endpoint.writer, &json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":route.host_id,"session_id":scope})).await.unwrap();
        let ready = read_json(&mut endpoint.reader, &mut endpoint.partial, MAX_LINE)
            .await
            .unwrap()
            .unwrap();
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
    // A reconnect cannot overlap a still-active process with the same input
    // and upload principal. It fails before ready and never starts its CLI.
    let (mut overlapping, server) = duplex(16384);
    let (read, write) = tokio::io::split(server);
    let denied = tokio::spawn(serve_io(storage.clone(), cli.clone(), read, write));
    write_json(&mut overlapping, &json!({"v":1,"type":"hello","client_id":route.client_id,"expected_host_id":route.host_id,"session_id":uuid::Uuid::new_v4().to_string()})).await.unwrap();
    assert!(
        timeout(Duration::from_secs(3), denied)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
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
    // The timer renews both independent leases while the SSH connection lives.
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
