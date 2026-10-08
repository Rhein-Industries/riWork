//! Orchestrators that run as a chat (docs/remote-protocol.md, "Chat orchestrators"):
//! `mode`, `chat_id` and `provider` on the entries of `orchestrators.list` and
//! `shells.list`, which of them the phone is shown, that an older CLI which has none
//! of them still answers exactly as before, that a malformed one is left out on its
//! own, and that the `shell.*` methods refuse a chat's id instead of driving it as a
//! tmux shell. Against a stub CLI that prints the JSON in `shells.json` and
//! `orchestrators.json` and logs every call.
use riwork_remote::{config::Storage, rpc::Rpc, viewport::Viewport};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn req(method: &str, params: Value) -> Value {
    json!({"v":1,"type":"request","id":new_uuid(),"method":method,"params":params})
}
fn code(response: &Value) -> &str {
    response["error"]["code"].as_str().unwrap_or("")
}
fn message(response: &Value) -> &str {
    response["error"]["message"].as_str().unwrap_or("")
}

/// What the phone is told when it drives a chat orchestrator as a terminal.
const CHAT_MESSAGE: &str =
    "this orchestrator runs as a chat; follow it with chat.events and send with chat.command";

/// How the stand-in CLI answers `capabilities --json`.
#[derive(Clone, Copy)]
enum Capabilities {
    /// A CLI that checks a shell itself: the connector lists no sessions first.
    Verifies,
    /// A stand-in that knows nothing of it: the connector looks sessions up.
    Silent,
}

struct Fixture {
    storage_dir: tempfile::TempDir,
    stub: tempfile::TempDir,
    rpc: Rpc,
    device: String,
}
impl Fixture {
    fn new(capabilities: Capabilities) -> Self {
        let storage_dir = tempfile::tempdir().unwrap();
        let stub = tempfile::tempdir().unwrap();
        let storage = Storage::at(storage_dir.path().into()).unwrap();
        let device = storage
            .pair(
                "wss://example.com/v1/ws".into(),
                "phone".into(),
                false,
                &storage_dir.path().join("phone.json"),
                None,
            )
            .unwrap()
            .device_id;
        Self {
            rpc: Rpc::new(stub_cli(stub.path(), capabilities), storage),
            storage_dir,
            stub,
            device,
        }
    }
    fn says(&self, file: &str, value: Value) {
        std::fs::write(self.stub.path().join(file), value.to_string()).unwrap();
    }
    /// A tmux shell the stand-in CLI knows: any other id it refuses as unknown.
    fn tmux_shell(&self, id: &str) {
        use std::io::Write;
        let mut live = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.stub.path().join("live"))
            .unwrap();
        writeln!(live, "{id}").unwrap();
    }
    async fn send(&self, method: &str, params: Value) -> Value {
        self.rpc
            .handle(&self.device, req(method, params))
            .await
            .unwrap()
    }
    async fn call(&self, method: &str, params: Value) -> Value {
        let response = self.send(method, params).await;
        assert_eq!(response["ok"], true, "{response}");
        response["result"].clone()
    }
    async fn orchestrators(&self) -> Value {
        self.call("orchestrators.list", json!({})).await["orchestrators"].clone()
    }
    /// Every shell method that takes a shell id, each with params that are valid
    /// for it, so the id is the only thing that can be refused.
    async fn drive(&self, shell: &str) -> Vec<(&'static str, Value)> {
        let mut viewport = Viewport::new(self.rpc.cli.clone(), self.device.clone());
        let mut responses = vec![
            (
                "shell.output",
                self.send("shell.output", json!({"shell_id":shell,"lines":10}))
                    .await,
            ),
            (
                "shell.history",
                self.send(
                    "shell.history",
                    json!({"shell_id":shell,"end":0,"lines":10}),
                )
                .await,
            ),
            (
                "shell.input",
                self.send("shell.input", json!({"shell_id":shell,"line":"echo hi"}))
                    .await,
            ),
            (
                "shell.keys",
                self.send(
                    "shell.keys",
                    json!({"shell_id":shell,"batch":new_uuid(),"items":[{"text":"x"}]}),
                )
                .await,
            ),
        ];
        for (method, params) in [
            (
                "shell.resize",
                json!({"shell_id":shell,"columns":43,"rows":17}),
            ),
            ("shell.resize.clear", json!({"shell_id":shell})),
        ] {
            let response = self
                .rpc
                .handle_in(&self.device, req(method, params), Some(&mut viewport))
                .await
                .unwrap();
            responses.push((method, response));
        }
        responses
    }
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.stub.path().join("argv.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| line.replace('\u{1f}', " ").trim().to_owned())
            .collect()
    }
    /// The calls that read from, typed into or sized a shell: not the lists, not
    /// the question about what the CLI checks.
    fn shell_commands(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|call| call.starts_with("shell ") && !call.starts_with("shell list"))
            .collect()
    }
    fn keys_ledger(&self) -> String {
        let path = self
            .storage_dir
            .path()
            .join("remote")
            .join(format!("keys-{}.json", self.device));
        std::fs::read_to_string(path).unwrap_or_default()
    }
    fn input_ledger(&self) -> String {
        let path = self
            .storage_dir
            .path()
            .join("remote")
            .join(format!("outcomes-{}.json", self.device));
        std::fs::read_to_string(path).unwrap_or_default()
    }
}

/// Logs every call and prints `shells.json` and `orchestrators.json` for the lists
/// (`[]` if missing, or a failure if `fail-lists` exists). As the real CLI does, it
/// refuses a shell id it does not know (`live` holds the ones it does) for
/// `shell output`, `shell history` and `shell keys`; the rest of the shell commands
/// do nothing and succeed.
fn stub_cli(dir: &Path, capabilities: Capabilities) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let cli = dir.join("fake-riwork");
    let capabilities = match capabilities {
        Capabilities::Verifies => "printf '{\"v\":1,\"verifies_shell\":true}'",
        Capabilities::Silent => "true",
    };
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\n\
             d='{dir}'\n\
             for a in \"$@\"; do printf '%s\\037' \"$a\"; done >> \"$d/argv.log\"\n\
             printf '\\n' >> \"$d/argv.log\"\n\
             list() {{\n\
               if [ -e \"$d/fail-lists\" ]; then echo 'riwork: list failed' >&2; exit 1; fi\n\
               if [ -e \"$d/$1\" ]; then cat \"$d/$1\"; else echo '[]'; fi\n\
             }}\n\
             refuse() {{\n\
               token=''; [ \"$1\" = keys ] && token='not_found: '\n\
               if ! grep -qx \"$2\" \"$d/live\" 2>/dev/null; then echo \"riwork: ${{token}}unknown shell $2\" >&2; exit 2; fi\n\
             }}\n\
             case \"$1 $2\" in\n\
             capabilities*) {capabilities};;\n\
             'shell list') list shells.json;;\n\
             'orchestrator list') list orchestrators.json;;\n\
             'shell output') refuse output \"$3\"; printf '{{\"id\":\"%s\",\"output\":\"hi\\\\n\"}}' \"$3\";;\n\
             'shell history') refuse history \"$3\"; printf '{{\"id\":\"%s\",\"output\":\"1\",\"line_count\":1,\"history_size\":5,\"complete\":false}}' \"$3\";;\n\
             'shell keys') refuse keys \"$3\";;\n\
             esac\n",
            dir = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}

/// A chat orchestrator as the CLI lists it, with the field only the CLI sees.
fn chat_orchestrator(id: &str, project: Option<&str>) -> Value {
    json!({
        "id": id,
        "project_id": project,
        "worktree_id": null,
        "kind": "orchestrator",
        "cwd": "/Users/me/code/app/orchestrator",
        "command": null,
        "harness": "codex",
        "unrestricted": false,
        "alive": true,
        "created_at_unix": 1790000000u64,
        "mode": "chat",
        "chat_id": id,
        "provider": "codex",
        "state": "idle",
        "activity": "done",
        "last_activity_unix": 1790000100u64
    })
}

/// A terminal orchestrator as an older CLI lists it: none of the three fields.
fn terminal_orchestrator(id: &str) -> Value {
    json!({
        "id": id,
        "project_id": null,
        "worktree_id": null,
        "kind": "orchestrator",
        "cwd": "/Users/me/code/app/orchestrator",
        "command": "codex --secret-flag",
        "harness": "codex",
        "unrestricted": true,
        "codex_account_id": "acct-secret",
        "alive": true,
        "created_at_unix": 1790000000u64
    })
}

/// What the phone is shown of `terminal_orchestrator`, before the three fields.
fn terminal_orchestrator_shown(id: &str) -> Value {
    json!({
        "id": id,
        "project_id": null,
        "worktree_id": null,
        "kind": "orchestrator",
        "cwd": "/Users/me/code/app/orchestrator",
        "harness": "codex",
        "alive": true,
        "created_at_unix": 1790000000u64
    })
}

#[tokio::test]
async fn a_chat_orchestrator_is_listed_with_its_mode_chat_id_and_provider() {
    let f = Fixture::new(Capabilities::Silent);
    let (global, project_id, project_chat) = (new_uuid(), new_uuid(), new_uuid());
    let project_orchestrator = chat_orchestrator(&project_chat, Some(&project_id));
    f.says(
        "orchestrators.json",
        json!([chat_orchestrator(&global, None), project_orchestrator]),
    );
    let listed = f.orchestrators().await;
    assert_eq!(
        listed[0],
        json!({
            "id": global, "project_id": null, "worktree_id": null, "kind": "orchestrator",
            "cwd": "/Users/me/code/app/orchestrator", "harness": "codex", "alive": true,
            "created_at_unix": 1790000000u64, "last_activity_unix": 1790000100u64,
            "activity": "done",
            "mode": "chat", "chat_id": global, "provider": "codex"
        })
    );
    // The CLI's own `state` is not the phone's: it follows the chat for that.
    assert!(listed[0].get("state").is_none());
    assert_eq!(listed[1]["project_id"], project_id);
    assert_eq!(listed[1]["chat_id"], project_chat);
    assert_eq!(listed[1]["id"], listed[1]["chat_id"]);
    // Claude is the other provider that passes.
    let mut claude = chat_orchestrator(&global, None);
    claude["provider"] = json!("claude");
    claude["harness"] = json!("claude");
    f.says("orchestrators.json", json!([claude]));
    assert_eq!(f.orchestrators().await[0]["provider"], "claude");
    // The same projection serves `shells.list`.
    f.says("shells.json", json!([chat_orchestrator(&global, None)]));
    let shells = f
        .call("shells.list", json!({"project_id": project_id}))
        .await;
    assert_eq!(shells["shells"][0]["mode"], "chat");
    assert_eq!(shells["shells"][0]["chat_id"], global);
    assert_eq!(shells["shells"][0]["provider"], "codex");
}

#[tokio::test]
async fn chat_id_and_provider_are_passed_on_only_for_an_entry_in_chat_mode() {
    let f = Fixture::new(Capabilities::Silent);
    let id = new_uuid();
    // A terminal entry that claims a chat, an entry without a mode that does, and
    // one whose mode is not one of the two words: what is shown of each.
    for (mode, shown) in [
        (Some("terminal"), Some("terminal")),
        (None, None),
        (Some("other"), None),
    ] {
        let mut entry = terminal_orchestrator(&id);
        entry["chat_id"] = json!(id);
        entry["provider"] = json!("codex");
        if let Some(mode) = mode {
            entry["mode"] = json!(mode);
        }
        f.says("orchestrators.json", json!([entry]));
        let mut expected = terminal_orchestrator_shown(&id);
        if let Some(shown) = shown {
            expected["mode"] = json!(shown);
        }
        assert_eq!(f.orchestrators().await, json!([expected]), "mode {mode:?}");
    }
}

#[tokio::test]
async fn a_malformed_mode_provider_or_chat_id_is_left_out_on_its_own() {
    let f = Fixture::new(Capabilities::Silent);
    let id = new_uuid();
    let canonical = new_uuid();
    let wrong_modes = [
        json!("other"),
        json!("Chat"),
        json!("CHAT"),
        json!(" chat"),
        json!(""),
        json!(null),
        json!(1),
        json!(true),
        json!(["chat"]),
        json!({"mode": "chat"}),
    ];
    for wrong in wrong_modes {
        let mut entry = chat_orchestrator(&id, None);
        entry["mode"] = wrong.clone();
        f.says("orchestrators.json", json!([entry]));
        let listed = &f.orchestrators().await[0];
        // No valid mode, so no chat: the two fields that describe one go with it.
        for field in ["mode", "chat_id", "provider"] {
            assert!(listed.get(field).is_none(), "mode={wrong}: {field} passed");
        }
        assert_eq!(listed["id"], id, "mode={wrong}");
        assert_eq!(listed["activity"], "done", "mode={wrong}");
    }
    let wrong_providers = [
        json!("grok"),
        json!("Codex"),
        json!("openai"),
        json!(""),
        json!(null),
        json!(2),
        json!(["codex"]),
    ];
    for wrong in wrong_providers {
        let mut entry = chat_orchestrator(&id, None);
        entry["provider"] = wrong.clone();
        f.says("orchestrators.json", json!([entry]));
        let listed = &f.orchestrators().await[0];
        assert!(
            listed.get("provider").is_none(),
            "provider={wrong} was passed on"
        );
        assert_eq!(listed["mode"], "chat", "provider={wrong}");
        assert_eq!(listed["chat_id"], id, "provider={wrong}");
    }
    let wrong_chat_ids = [
        json!("not-a-uuid"),
        json!(""),
        json!(canonical.to_uppercase()),
        json!(canonical.replace('-', "")),
        json!(format!("{{{canonical}}}")),
        json!(format!("urn:uuid:{canonical}")),
        json!(format!(" {canonical}")),
        json!(&canonical[..8]),
        json!(null),
        json!(7),
        json!([canonical]),
    ];
    for wrong in wrong_chat_ids {
        let mut entry = chat_orchestrator(&id, None);
        entry["chat_id"] = wrong.clone();
        f.says("orchestrators.json", json!([entry]));
        let listed = &f.orchestrators().await[0];
        assert!(
            listed.get("chat_id").is_none(),
            "chat_id={wrong} was passed on"
        );
        assert_eq!(listed["mode"], "chat", "chat_id={wrong}");
        assert_eq!(listed["provider"], "codex", "chat_id={wrong}");
    }
    // One malformed entry does not fail the list or touch its neighbours.
    let (bad, good) = (new_uuid(), new_uuid());
    let mut broken = chat_orchestrator(&bad, None);
    broken["chat_id"] = json!("nope");
    broken["provider"] = json!("grok");
    f.says(
        "orchestrators.json",
        json!([broken, chat_orchestrator(&good, None)]),
    );
    let listed = f.orchestrators().await;
    assert_eq!(listed[0]["mode"], "chat");
    assert!(listed[0].get("chat_id").is_none() && listed[0].get("provider").is_none());
    assert_eq!(listed[1]["chat_id"], good);
    assert_eq!(listed[1]["provider"], "codex");
}

#[tokio::test]
async fn every_shell_method_refuses_a_chat_orchestrator_before_any_shell_command() {
    // The CLI does not say it checks shells: the connector looks the id up.
    let f = Fixture::new(Capabilities::Silent);
    let chat = new_uuid();
    f.says(
        "orchestrators.json",
        json!([chat_orchestrator(&chat, None)]),
    );
    for (method, response) in f.drive(&chat).await {
        assert_eq!(response["ok"], false, "{method}: {response}");
        assert_eq!(code(&response), "invalid_request", "{method}: {response}");
        assert_eq!(message(&response), CHAT_MESSAGE, "{method}");
    }
    // Nothing was read, typed or sized: only lists were asked for.
    assert_eq!(f.shell_commands(), Vec::<String>::new(), "{:?}", f.calls());
    // And nothing was written down as sent or pending.
    assert!(f.input_ledger().is_empty(), "{}", f.input_ledger());
    let ledger: Value = serde_json::from_str(&f.keys_ledger()).unwrap_or(json!({"batches":[]}));
    assert_eq!(ledger["batches"], json!([]), "{ledger}");
}

#[tokio::test]
async fn a_cli_that_checks_shells_itself_is_still_told_apart_from_a_chat_orchestrator() {
    let f = Fixture::new(Capabilities::Verifies);
    let chat = new_uuid();
    f.says(
        "orchestrators.json",
        json!([chat_orchestrator(&chat, None)]),
    );
    for (method, response) in f.drive(&chat).await {
        assert_eq!(response["ok"], false, "{method}: {response}");
        assert_eq!(code(&response), "invalid_request", "{method}: {response}");
        assert_eq!(message(&response), CHAT_MESSAGE, "{method}");
    }
    // Reads and keys go to the CLI, which refuses the id as one it does not
    // know, once each; a typed line and a resize are looked up first and never
    // reach it.
    assert_eq!(
        f.shell_commands()
            .iter()
            .map(|c| c.split(' ').take(2).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>(),
        ["shell output", "shell history", "shell keys"]
    );
    assert!(f.input_ledger().is_empty(), "{}", f.input_ledger());
    // The batch the CLI refused was forgotten, so it can never read as uncertain.
    let ledger: Value = serde_json::from_str(&f.keys_ledger()).unwrap_or(json!({"batches":[]}));
    assert_eq!(ledger["batches"], json!([]), "{ledger}");
}

#[tokio::test]
async fn terminal_orchestrators_and_entries_without_a_mode_are_still_driven_as_shells() {
    for capabilities in [Capabilities::Silent, Capabilities::Verifies] {
        let f = Fixture::new(capabilities);
        let (legacy, terminal) = (new_uuid(), new_uuid());
        let mut marked = terminal_orchestrator(&terminal);
        marked["mode"] = json!("terminal");
        f.says(
            "orchestrators.json",
            json!([terminal_orchestrator(&legacy), marked]),
        );
        for shell in [&legacy, &terminal] {
            f.tmux_shell(shell);
            for (method, response) in f.drive(shell).await {
                assert_eq!(response["ok"], true, "{method} {shell}: {response}");
            }
        }
    }
}

#[tokio::test]
async fn an_unknown_id_is_not_found_whatever_the_cli_checks() {
    for capabilities in [Capabilities::Silent, Capabilities::Verifies] {
        let f = Fixture::new(capabilities);
        f.says(
            "orchestrators.json",
            json!([chat_orchestrator(&new_uuid(), None)]),
        );
        for (method, response) in f.drive(&new_uuid()).await {
            assert_eq!(code(&response), "not_found", "{method}: {response}");
            assert_eq!(
                message(&response),
                "existing shell ID not found",
                "{method}"
            );
        }
    }
}

#[tokio::test]
async fn a_lookup_that_cannot_be_made_leaves_the_clis_refusal_as_it_was() {
    let f = Fixture::new(Capabilities::Verifies);
    let chat = new_uuid();
    f.says(
        "orchestrators.json",
        json!([chat_orchestrator(&chat, None)]),
    );
    std::fs::write(f.stub.path().join("fail-lists"), "").unwrap();
    let response = f
        .send("shell.output", json!({"shell_id":chat,"lines":10}))
        .await;
    assert_eq!(code(&response), "not_found", "{response}");
    assert_eq!(message(&response), "existing shell ID not found");
}
