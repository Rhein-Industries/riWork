use riwork_remote::{
    config::{Storage, private_write},
    crypto::uuid,
    rpc::{Request, Rpc},
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
        Rpc {
            cli: "/nonexistent/no-CLI-may-be-executed".into(),
            storage,
        },
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
    ] {
        let result = rpc.handle(&device, r).await.unwrap();
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
