//! A CLI that checks for itself that a shell exists and is alive
//! (`riwork capabilities` says so) is not asked to list sessions first: one CLI
//! process per request instead of two or three. What a phone is told stays
//! what it was told, and a CLI that does not say so is handled as before.
use riwork_remote::{config::Storage, rpc::Rpc};
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

/// How the stand-in CLI answers `capabilities --json`.
#[derive(Clone, Copy)]
enum Capabilities {
    /// What a current CLI prints.
    Verifies,
    /// A CLI from before the command: it refuses it and exits with an error.
    Unknown,
    /// A stand-in that knows nothing of it and prints nothing.
    Silent,
    /// A current CLI that says it does not check.
    Denies,
}

struct Fixture {
    _storage_dir: tempfile::TempDir,
    stub: tempfile::TempDir,
    rpc: Rpc,
    device: String,
    shell: String,
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
        let shell = new_uuid();
        let cli = stub_cli(stub.path(), &shell, capabilities);
        Self {
            rpc: Rpc::new(cli, storage),
            _storage_dir: storage_dir,
            stub,
            device,
            shell,
        }
    }
    async fn call(&self, method: &str, params: Value) -> Value {
        self.rpc
            .handle(&self.device, req(method, params))
            .await
            .unwrap()
    }
    async fn output(&self, shell: &str) -> Value {
        self.call("shell.output", json!({"shell_id":shell,"lines":10}))
            .await
    }
    async fn history(&self, shell: &str) -> Value {
        self.call(
            "shell.history",
            json!({"shell_id":shell,"end":0,"lines":10}),
        )
        .await
    }
    async fn keys(&self, shell: &str) -> Value {
        self.call(
            "shell.keys",
            json!({"shell_id":shell,"batch":new_uuid(),"items":[{"text":"x"}]}),
        )
        .await
    }
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.stub.path().join("argv.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| line.replace('\u{1f}', " ").trim().to_owned())
            .collect()
    }
    fn calls_with(&self, prefix: &str) -> usize {
        self.calls()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }
    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.stub.path().join(name), value).unwrap();
    }
    fn ledger(&self) -> String {
        let path = self
            ._storage_dir
            .path()
            .join("remote")
            .join(format!("keys-{}.json", self.device));
        std::fs::read_to_string(path).unwrap_or_default()
    }
}

/// A CLI that logs every call and, as the real one does for `shell output`,
/// `shell history` and `shell keys`, refuses a shell it does not know (stderr
/// `riwork: unknown shell ID`, with the `not_found: ` token for keys) or one
/// that has exited (a `dead` file).
fn stub_cli(dir: &Path, shell: &str, capabilities: Capabilities) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let cli = dir.join("fake-riwork");
    let capabilities = match capabilities {
        Capabilities::Verifies => "printf '{\"v\":1,\"verifies_shell\":true}'",
        Capabilities::Unknown => "echo \"riwork: Unknown invocation 'capabilities'\" >&2; exit 2",
        Capabilities::Silent => "true",
        Capabilities::Denies => "printf '{\"v\":1,\"verifies_shell\":false}'",
    };
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\n\
             d='{dir}'\n\
             for a in \"$@\"; do printf '%s\\037' \"$a\"; done >> \"$d/argv.log\"\n\
             printf '\\n' >> \"$d/argv.log\"\n\
             refuse() {{\n\
               token=''; [ \"$1\" = keys ] && token='not_found: '\n\
               if [ \"$2\" != '{shell}' ]; then echo \"riwork: ${{token}}unknown shell $2\" >&2; exit 2; fi\n\
               if [ -e \"$d/dead\" ]; then echo \"riwork: ${{token}}shell $2 has exited\" >&2; exit 2; fi\n\
               if [ -e \"$d/other\" ]; then echo \"$(cat \"$d/other\")\" >&2; exit 2; fi\n\
             }}\n\
             case \"$1 $2\" in\n\
             capabilities*) {capabilities};;\n\
             'shell list') if [ -e \"$d/dead\" ]; then alive=false; else alive=true; fi; printf '[{{\"id\":\"{shell}\",\"alive\":%s}}]' \"$alive\";;\n\
             'orchestrator list') echo '[]';;\n\
             'shell output') refuse output \"$3\"; printf '{{\"id\":\"%s\",\"output\":\"hi\\\\n\",\"hash\":\"0123456789abcdef\"}}' \"$3\";;\n\
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

#[tokio::test]
async fn a_cli_that_checks_shells_is_asked_once_and_no_sessions_are_listed() {
    let f = Fixture::new(Capabilities::Verifies);
    for _ in 0..3 {
        assert_eq!(f.output(&f.shell).await["ok"], true);
        assert_eq!(f.history(&f.shell).await["ok"], true);
        assert_eq!(f.keys(&f.shell).await["result"]["status"], "sent");
    }
    assert_eq!(f.calls_with("capabilities"), 1, "{:?}", f.calls());
    assert_eq!(f.calls_with("shell list"), 0, "{:?}", f.calls());
    assert_eq!(f.calls_with("orchestrator list"), 0, "{:?}", f.calls());
    assert_eq!(f.calls_with("shell output"), 3);
    assert_eq!(f.calls_with("shell history"), 3);
    assert_eq!(f.calls_with("shell keys"), 3);
    // One CLI process each, plus the one question.
    assert_eq!(f.calls().len(), 10);
}

#[tokio::test]
async fn a_refusal_of_the_cli_reads_like_the_lookup_it_replaces() {
    let checking = Fixture::new(Capabilities::Verifies);
    let listing = Fixture::new(Capabilities::Silent);
    let unknown = new_uuid();
    for f in [&checking, &listing] {
        // Unknown to the registry.
        for response in [
            f.output(&unknown).await,
            f.history(&unknown).await,
            f.keys(&unknown).await,
        ] {
            assert_eq!(code(&response), "not_found", "{response}");
            assert_eq!(message(&response), "existing shell ID not found");
        }
        // Registered, but its tmux session is gone.
        f.set("dead", "");
        for response in [
            f.output(&f.shell).await,
            f.history(&f.shell).await,
            f.keys(&f.shell).await,
        ] {
            assert_eq!(code(&response), "not_found", "{response}");
            assert_eq!(message(&response), "selected shell is not alive");
        }
        std::fs::remove_file(f.stub.path().join("dead")).unwrap();
        // The refused batches left nothing behind to be reported as uncertain.
        let ledger: Value = serde_json::from_str(&f.ledger()).unwrap_or(json!({"batches":[]}));
        assert_eq!(ledger["batches"], json!([]), "{ledger}");
        assert_eq!(f.keys(&f.shell).await["result"]["status"], "sent");
    }
    // The lookup was made where the CLI does not check. Where it does, only an
    // id it refused as unknown is looked up afterwards (to tell a chat
    // orchestrator from an id nobody has: tests/chat_orchestrator.rs), once for
    // each of the three refusals above and for nothing else: not for a shell
    // that exists or one that has exited.
    assert_eq!(checking.calls_with("shell list"), 3);
    assert_eq!(checking.calls_with("orchestrator list"), 3);
    assert!(listing.calls_with("shell list") > 0);
}

#[tokio::test]
async fn only_the_clis_own_refusal_is_put_in_other_words() {
    let f = Fixture::new(Capabilities::Verifies);
    // Another failure keeps its words and its code.
    f.set("other", "riwork: tmux: no server running");
    let response = f.output(&f.shell).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    assert!(
        message(&response).contains("no server running"),
        "{response}"
    );
    let response = f.history(&f.shell).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    // Words that merely contain the phrase are not the refusal.
    f.set(
        "other",
        &format!("riwork: tmux said unknown shell {} twice", f.shell),
    );
    let response = f.output(&f.shell).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    assert!(message(&response).contains("tmux said"), "{response}");
    // Nor is the refusal of a different shell.
    f.set("other", &format!("riwork: unknown shell {}", new_uuid()));
    let response = f.output(&f.shell).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    // A bad id never reaches the CLI at all.
    let before = f.calls().len();
    let response = f.output("nope").await;
    assert_eq!(code(&response), "invalid_request", "{response}");
    assert_eq!(f.calls().len(), before);
}

#[tokio::test]
async fn a_cli_that_does_not_say_it_checks_is_asked_once_and_sessions_are_listed() {
    for capabilities in [
        Capabilities::Unknown,
        Capabilities::Silent,
        Capabilities::Denies,
    ] {
        let f = Fixture::new(capabilities);
        for _ in 0..3 {
            assert_eq!(f.output(&f.shell).await["ok"], true);
            assert_eq!(f.history(&f.shell).await["ok"], true);
            assert_eq!(f.keys(&f.shell).await["result"]["status"], "sent");
        }
        // The answer, a negative one included, is kept.
        assert_eq!(f.calls_with("capabilities"), 1, "{:?}", f.calls());
        assert_eq!(f.calls_with("shell list"), 9);
        // And a dead shell is refused by the lookup before the CLI is asked.
        f.set("dead", "");
        let before = f.calls_with("shell output");
        let response = f.output(&f.shell).await;
        assert_eq!(message(&response), "selected shell is not alive");
        assert_eq!(f.calls_with("shell output"), before);
    }
}

#[tokio::test]
async fn a_cli_that_could_not_be_run_is_not_remembered_as_anything() {
    let f = Fixture::new(Capabilities::Verifies);
    let real = f.rpc.cli.clone();
    let moved = real.with_file_name("moved-riwork");
    std::fs::rename(&real, &moved).unwrap();
    let response = f.output(&f.shell).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    std::fs::rename(&moved, &real).unwrap();
    // Found again: it is asked now, and the answer is a yes.
    assert_eq!(f.output(&f.shell).await["ok"], true);
    assert_eq!(f.calls_with("capabilities"), 1);
    assert_eq!(f.calls_with("shell list"), 0);
}

#[tokio::test]
async fn input_and_resizing_still_look_the_shell_up() {
    // Typed lines and geometry are not on the hot path and keep the lookup.
    let f = Fixture::new(Capabilities::Verifies);
    let response = f
        .call("shell.input", json!({"shell_id":f.shell,"line":"echo hi"}))
        .await;
    assert_eq!(response["result"]["status"], "sent", "{response}");
    assert!(f.calls_with("shell list") > 0);
    let unknown = f
        .call(
            "shell.input",
            json!({"shell_id":new_uuid(),"line":"echo hi"}),
        )
        .await;
    assert_eq!(code(&unknown), "not_found", "{unknown}");
}
