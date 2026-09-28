use riwork_remote::{
    config::{Storage, private_write},
    crypto::uuid,
    rpc::{Request, Rpc},
    viewport::Viewport,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
fn fixture() -> (tempfile::TempDir, Rpc, String) {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::at(tmp.path().into()).unwrap();
    let pair = storage
        .pair(
            "wss://example.com/v1/ws".into(),
            "phone".into(),
            false,
            &tmp.path().join("pair.json"),
            None,
        )
        .unwrap();
    (
        tmp,
        Rpc::new("/nonexistent/no-CLI-may-be-executed".into(), storage),
        pair.device_id,
    )
}
fn req(method: &str, params: Value) -> Value {
    json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":method,"params":params})
}
#[tokio::test]
async fn untrusted_rpc_shape_and_input_limits_fail_before_cli() {
    let (_tmp, rpc, device) = fixture();
    let shell = uuid::Uuid::new_v4().to_string();
    let mut viewport = Viewport::new(rpc.cli.clone(), device.clone());
    for r in [
        req("execute", json!({"command":"anything"})),
        req("projects.list", json!({"unexpected":true})),
        req("worktrees.list", json!({})),
        req("shell.output", json!({"shell_id":shell,"lines":0})),
        req("shell.output", json!({"shell_id":shell,"lines":2001})),
        req("shell.input", json!({"shell_id":shell,"line":"one\rtwo"})),
        req("shell.input", json!({"shell_id":shell,"line":"\u{1b}"})),
        req("shell.input", json!({"shell_id":shell,"line":"a\u{2028}b"})),
        req(
            "shell.input",
            json!({"shell_id":shell,"line":"x".repeat(8193)}),
        ),
        req(
            "shell.input",
            json!({"shell_id":shell,"line":"ok","command":"forbidden"}),
        ),
        req("shell.output", json!({"shell_id":"12345678"})),
        req(
            "shell.resize",
            json!({"shell_id":shell,"columns":19,"rows":17}),
        ),
        req(
            "shell.resize",
            json!({"shell_id":shell,"columns":301,"rows":17}),
        ),
        req(
            "shell.resize",
            json!({"shell_id":shell,"columns":43,"rows":7}),
        ),
        req(
            "shell.resize",
            json!({"shell_id":shell,"columns":43,"rows":161}),
        ),
        req(
            "shell.resize",
            json!({"shell_id":shell,"columns":43.0,"rows":17}),
        ),
        req(
            "shell.resize",
            json!({"shell_id":shell,"columns":null,"rows":17}),
        ),
        req(
            "shell.resize",
            json!({"shell_id":shell,"columns":43,"rows":17,"owner":device}),
        ),
        req("shell.resize.clear", json!({"shell_id":"short"})),
        req(
            "shell.resize.clear",
            json!({"shell_id":shell,"lease":device}),
        ),
    ] {
        let result = rpc
            .handle_in(&device, r, Some(&mut viewport))
            .await
            .unwrap();
        assert_eq!(result["error"]["code"], "invalid_request", "{result}");
    }
    assert!(uuid("11111111-1111-4111-8111-11111111111A").is_err());
    rpc.storage.revoke(&device).unwrap();
    assert!(
        rpc.handle(&device, req("projects.list", json!({})))
            .await
            .is_err()
    );
}
#[tokio::test]
async fn persistent_cached_conflicting_pending_and_capacity_outcomes_never_execute_cli() {
    let (_tmp, rpc, device) = fixture();
    let shell = uuid::Uuid::new_v4().to_string();
    let r = req("shell.input", json!({"shell_id":shell,"line":"test"}));
    let id = r["id"].as_str().unwrap();
    let typed: Request = serde_json::from_value(r.clone()).unwrap();
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(&typed).unwrap()));
    let path = rpc.storage.dir.join(format!("outcomes-{device}.json"));
    let cached = json!({"v":1,"type":"response","id":id,"ok":true,"result":{"shell_id":shell,"status":"sent"}});
    let mut ledger = json!({"entries":{id:{"digest":digest,"response":cached}}});
    private_write(&path, &ledger).unwrap();
    assert_eq!(rpc.handle(&device, r.clone()).await.unwrap(), cached);
    let mut conflict = r.clone();
    conflict["params"]["line"] = json!("different");
    assert_eq!(
        rpc.handle(&device, conflict).await.unwrap()["error"]["code"],
        "request_conflict"
    );
    let mut conflict = r.clone();
    conflict["method"] = json!("projects.list");
    conflict["params"] = json!({});
    assert_eq!(
        rpc.handle(&device, conflict).await.unwrap()["error"]["code"],
        "request_conflict"
    );
    ledger["entries"][id]["response"] = Value::Null;
    private_write(&path, &ledger).unwrap();
    assert_eq!(
        rpc.handle(&device, r.clone()).await.unwrap()["error"]["code"],
        "outcome_unknown"
    );
    let cached_error = json!({"v":1,"type":"response","id":id,"ok":false,"error":{"code":"outcome_unknown","message":"exact persisted outcome message"}});
    ledger["entries"][id]["response"] = cached_error.clone();
    private_write(&path, &ledger).unwrap();
    assert_eq!(rpc.handle(&device, r).await.unwrap(), cached_error);
    let mut entries = serde_json::Map::new();
    for _ in 0..4096 {
        entries.insert(
            uuid::Uuid::new_v4().to_string(),
            json!({"digest":"test","response":null}),
        );
    }
    private_write(&path, &json!({"entries":entries})).unwrap();
    let full = rpc
        .handle(
            &device,
            req(
                "shell.input",
                json!({"shell_id":shell,"line":"capacity test"}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(full["error"]["code"], "cache_full");
}

/// A scripted stand-in for the RiWork CLI that logs every invocation. `shell` is
/// the one live shell it reports (until a `dead` file appears beside the log);
/// its `shell output` floods stdout past the limit.
#[cfg(unix)]
fn fake_cli(dir: &std::path::Path, shell: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let log = dir.join("cli.log");
    let cli = dir.join("fake-riwork");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1 $2\" in\n\
             'shell list') if [ -e '{}' ]; then echo '[]'; else printf '[{{\"id\":\"{shell}\",\"alive\":true}}]'; fi;;\n\
             'orchestrator list') echo '[]';;\n\
             'shell output') head -c 200000 /dev/zero | tr '\\0' a;;\nesac\n",
            log.display(),
            dir.join("dead").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    (cli, log)
}
#[cfg(unix)]
fn calls(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}
#[tokio::test]
async fn malformed_request_ids_get_an_error_response_instead_of_dropping_the_session() {
    let (_tmp, rpc, device) = fixture();
    for value in [
        json!({"v":1,"type":"request","method":"projects.list","params":{}}),
        json!({"v":1,"type":"request","id":7,"method":"projects.list","params":{}}),
        json!({"v":1,"type":"request","id":null,"method":"projects.list","params":{}}),
        json!({"v":1,"type":"request","id":"x".repeat(65),"method":"projects.list","params":{}}),
        json!("not an object"),
        Value::Null,
    ] {
        let response = rpc.handle(&device, value).await.unwrap();
        assert_eq!(response["ok"], false, "{response}");
        assert_eq!(response["error"]["code"], "invalid_request", "{response}");
        assert_eq!(response["id"], Value::Null, "{response}");
    }
    // A string ID that is not a UUID is still echoed so the caller can correlate.
    let response = rpc
        .handle(
            &device,
            json!({"v":1,"type":"request","id":"not-a-uuid","method":"projects.list","params":{}}),
        )
        .await
        .unwrap();
    assert_eq!(response["error"]["code"], "invalid_request");
    assert_eq!(response["id"], "not-a-uuid");
}
#[cfg(unix)]
#[tokio::test]
async fn cli_output_over_the_limit_is_response_too_large_not_cli_error() {
    let (tmp, _, device) = fixture();
    let shell = uuid::Uuid::new_v4().to_string();
    let (cli, _log) = fake_cli(tmp.path(), &shell);
    let rpc = Rpc::new(cli, Storage::at(tmp.path().into()).unwrap());
    let response = rpc
        .handle(&device, req("shell.output", json!({"shell_id":shell})))
        .await
        .unwrap();
    assert_eq!(
        response["error"]["code"], "response_too_large",
        "{response}"
    );
}
#[cfg(unix)]
#[tokio::test]
async fn resize_clear_checks_the_session_before_reaching_the_cli() {
    let (tmp, _, device) = fixture();
    let live = uuid::Uuid::new_v4().to_string();
    let (cli, log) = fake_cli(tmp.path(), &live);
    let rpc = Rpc::new(cli.clone(), Storage::at(tmp.path().into()).unwrap());
    let mut viewport = Viewport::new(cli, device.clone());
    // Each fresh UUID would leave a permanent lock file in the CLI's terminal-control dir.
    for _ in 0..3 {
        let unknown = uuid::Uuid::new_v4().to_string();
        let response = rpc
            .handle_in(
                &device,
                req("shell.resize.clear", json!({"shell_id":unknown})),
                Some(&mut viewport),
            )
            .await
            .unwrap();
        assert_eq!(response["error"]["code"], "not_found", "{response}");
    }
    assert!(
        calls(&log).iter().all(|c| !c.contains("resize-clear")),
        "{:?}",
        calls(&log)
    );
    // A live shell reaches the CLI (idempotent clear).
    let response = rpc
        .handle_in(
            &device,
            req("shell.resize.clear", json!({"shell_id":live})),
            Some(&mut viewport),
        )
        .await
        .unwrap();
    assert_eq!(response["result"]["status"], "cleared", "{response}");
    assert_eq!(
        calls(&log)
            .iter()
            .filter(|c| c.contains("resize-clear"))
            .count(),
        1
    );
    // A shell this connection pinned and that has since died can still be released.
    let response = rpc
        .handle_in(
            &device,
            req(
                "shell.resize",
                json!({"shell_id":live,"columns":43,"rows":17}),
            ),
            Some(&mut viewport),
        )
        .await
        .unwrap();
    assert_eq!(response["ok"], true, "{response}");
    std::fs::write(tmp.path().join("dead"), "").unwrap();
    let before = calls(&log).len();
    let response = rpc
        .handle_in(
            &device,
            req("shell.resize.clear", json!({"shell_id":live})),
            Some(&mut viewport),
        )
        .await
        .unwrap();
    assert_eq!(response["result"]["status"], "cleared", "{response}");
    assert!(
        calls(&log)[before..]
            .iter()
            .all(|c| c.starts_with("shell resize-clear")),
        "an owned clear needs no session lookup: {:?}",
        calls(&log)
    );
}
