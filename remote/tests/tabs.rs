//! Full RPC dispatch against a CLI fixture: calls, wire output and validation.
use riwork_remote::{config::Storage, rpc::Rpc};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
fn request(method: &str, params: Value) -> Value {
    json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":method,"params":params})
}
#[tokio::test]
async fn both_tab_calls_forward_structured_args_and_share_a_list() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("storage")).unwrap();
    let storage = Storage::at(dir.path().join("storage")).unwrap();
    let device = storage
        .pair(
            "wss://example.com/v1/ws".into(),
            "phone".into(),
            false,
            &dir.path().join("phone.json"),
            None,
        )
        .unwrap()
        .device_id;
    let cli = dir.path().join("cli");
    std::fs::write(
        &cli,
        format!(
            r#"#!/bin/sh
if [ "$1" = capabilities ]; then echo '{{"v":1,"tabs":true}}'; exit; fi
printf '%s\n' "$@" >> '{}'
case "$6" in *'"action":"hide"'*) echo '{{"error":{{"code":"not_found","message":"structured refusal"}}}}'; exit;; esac
echo '{{"entries":[]}}'
"#,
            dir.path().join("calls").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let rpc = Rpc::new(cli, storage);
    let project = uuid::Uuid::new_v4().to_string();
    let key = format!("chat:{}", uuid::Uuid::new_v4());
    let list = rpc
        .handle(&device, request("tabs.list", json!({"project_id":project})))
        .await
        .unwrap();
    assert_eq!(list["result"], json!({"entries":[]}));
    let update=rpc.handle(&device,request("tabs.update",json!({"project_id":project,"update":{"action":"rename","key":key,"title":"A title $(literal)"}}))).await.unwrap();
    assert_eq!(update["result"], json!({"entries":[]}));
    let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
    assert!(calls.contains("tabs\nlist\n--project\n"));
    assert!(calls.contains("tabs\nupdate\n--project\n"));
    assert!(calls.contains("A title $(literal)"));
    assert_eq!(calls.lines().filter(|line| *line == "--json").count(), 2);
    let missing = rpc
        .handle(
            &device,
            request(
                "tabs.update",
                json!({"project_id":project,"update":{"action":"hide","key":key}}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(missing["error"]["code"], "not_found");
    assert_eq!(missing["error"]["message"], "structured refusal");
    let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
    let bad = rpc
        .handle(
            &device,
            request(
                "tabs.update",
                json!({"project_id":project,"update":{"action":"hide","key":"x"}}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(bad["error"]["code"], "invalid_request");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("calls")).unwrap(),
        calls
    );
}
