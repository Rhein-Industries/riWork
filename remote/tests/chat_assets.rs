#![cfg(unix)]
//! Real RPC dispatch, a deterministic CLI stub, and temporary workspace files only.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use riwork_remote::{config::Storage, rpc::Rpc};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::PathBuf};

struct Fixture {
    dir: tempfile::TempDir,
    rpc: Rpc,
    device: String,
    chat: String,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("storage")).unwrap();
        std::fs::set_permissions(
            dir.path().join("storage"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
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
        let chat = uuid::Uuid::new_v4().to_string();
        let root = dir.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let cli = dir.path().join("cli");
        std::fs::write(&cli, "#!/bin/sh\nd=$(dirname \"$0\")\nprintf '%s\\n' \"$2\" >> \"$d/calls\"\ncase \"$1 $2\" in\n'chat snapshot') cat \"$d/snapshot.json\";;\n'chat list') cat \"$d/list.json\";;\n*) exit 2;;\nesac\n").unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            dir.path().join("list.json"),
            json!([{"id":chat,"cwd":root}]).to_string(),
        )
        .unwrap();
        std::fs::write(dir.path().join("snapshot.json"), json!({"v":1,"chat_id":chat,"cursor":"abcd-1234","next":5,"more":false,"before":1,"controls":[],"items":[{"order":1,"item":{"id":"message","status":"completed","body":{"type":"agent_message","text":"![Plot](plot.png) [Escape](../secret.txt)"}}}]}).to_string()).unwrap();
        Self {
            dir,
            rpc: Rpc::new(cli, storage),
            device,
            chat,
            root,
        }
    }
    fn request(&self, reference: &str, offset: usize) -> Value {
        json!({"v":1,"type":"request","id":uuid::Uuid::new_v4().to_string(),"method":"chat.asset.read","params":{"chat_id":self.chat,"cursor":"abcd-1234","item_id":"message","reference":reference,"offset":offset}})
    }
    async fn read(&self, reference: &str, offset: usize) -> Value {
        self.rpc
            .handle(&self.device, self.request(reference, offset))
            .await
            .unwrap()
    }
    fn calls(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .count()
    }
}
#[tokio::test]
async fn chat_assets_snapshot_bytes_once_and_never_mix_changed_file_chunks() {
    let f = Fixture::new();
    let original = vec![42; 65536 + 7];
    std::fs::write(f.root.join("plot.png"), &original).unwrap();
    let first = f.read("plot.png", 0).await;
    assert!(first.get("error").is_none(), "{first}");
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(first["result"]["data"].as_str().unwrap())
            .unwrap(),
        original[..65536]
    );
    assert_eq!(
        f.calls(),
        2,
        "one bounded item snapshot and one trusted chat lookup"
    );
    std::fs::write(f.root.join("plot.png"), b"changed").unwrap();
    let second = f.read("plot.png", 65536).await;
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(second["result"]["data"].as_str().unwrap())
            .unwrap(),
        original[65536..]
    );
    assert_eq!(first["result"]["sha256"], second["result"]["sha256"]);
    assert_eq!(
        f.calls(),
        2,
        "later chunks use the immutable bounded transfer cache"
    );
    f.rpc.storage.revoke(&f.device).unwrap();
    assert!(
        f.rpc
            .handle(&f.device, f.request("plot.png", 65536))
            .await
            .is_err(),
        "revocation applies even to cached chunks"
    );
}
#[tokio::test]
async fn chat_assets_refuse_unreferenced_paths_workspace_escape_and_unknown_transfers() {
    let f = Fixture::new();
    std::fs::write(f.root.join("plot.png"), b"picture").unwrap();
    std::fs::write(f.dir.path().join("secret.txt"), b"secret").unwrap();
    assert_eq!(
        f.read("plot.png", 65536).await["error"]["code"],
        "asset_expired"
    );
    assert_eq!(f.calls(), 0);
    assert_eq!(
        f.read("https://example.com/p.png", 0).await["error"]["code"],
        "invalid_request"
    );
    assert_eq!(f.calls(), 0);
    assert_eq!(
        f.read("../secret.txt", 0).await["error"]["code"],
        "asset_unavailable"
    );
    assert_eq!(
        f.read("unmentioned.txt", 0).await["error"]["code"],
        "asset_unavailable"
    );
}
