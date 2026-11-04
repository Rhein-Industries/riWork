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
