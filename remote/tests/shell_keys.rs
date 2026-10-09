//! `shell.keys` (direct typing) and the screen fields of `shell.output`, against
//! a stub CLI that records its argv. The production key module is compiled here
//! too, without GPUI, to keep the two validators identical and to type into a
//! real private tmux.
#[allow(dead_code)]
#[path = "../../src/session_keys.rs"]
mod session_keys;
#[allow(dead_code)]
#[path = "../../src/session_viewport.rs"]
mod session_viewport;

use riwork_remote::{
    config::{Storage, private_read, private_write},
    rpc::{KEYS_LEDGER_MAX, Rpc},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn req(method: &str, params: Value) -> Value {
    json!({"v":1,"type":"request","id":new_uuid(),"method":method,"params":params})
}
fn keys_req(shell: &str, batch: &str, items: Value) -> Value {
    req(
        "shell.keys",
        json!({"shell_id":shell,"batch":batch,"items":items}),
    )
}
fn pair(storage: &Storage, dir: &Path, name: &str) -> String {
    storage
        .pair(
            "wss://example.com/v1/ws".into(),
            name.into(),
            false,
            &dir.join(format!("{name}.json")),
            None,
        )
        .unwrap()
        .device_id
}

/// A storage directory, a paired device, and (optionally) a stub CLI.
struct Fixture {
    _storage_dir: tempfile::TempDir,
    stub: tempfile::TempDir,
    rpc: Rpc,
    device: String,
    shell: String,
}
impl Fixture {
    fn new() -> Self {
        let storage_dir = tempfile::tempdir().unwrap();
        let stub = tempfile::tempdir().unwrap();
        let storage = Storage::at(storage_dir.path().into()).unwrap();
        let device = pair(&storage, storage_dir.path(), "phone");
        let shell = new_uuid();
        let cli = stub_cli(stub.path(), &shell);
        Self {
            rpc: Rpc::new(cli, storage),
            _storage_dir: storage_dir,
            stub,
            device,
            shell,
        }
    }
    /// Without any CLI: every request must be answered before one would run.
    fn without_cli() -> Self {
        let mut fixture = Self::new();
        fixture.rpc = Rpc::new(
            "/nonexistent/no-CLI-may-be-executed".into(),
            fixture.rpc.storage.clone(),
        );
        fixture
    }
    async fn call(&self, request: Value) -> Value {
        self.rpc.handle(&self.device, request).await.unwrap()
    }
    async fn keys(&self, batch: &str, items: Value) -> Value {
        self.call(keys_req(&self.shell, batch, items)).await
    }
    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.stub.path().join(name), value).unwrap();
    }
    fn ledger_path(&self) -> PathBuf {
        self.rpc
            .storage
            .dir
            .join(format!("keys-{}.json", self.device))
    }
    fn ledger(&self) -> Value {
        private_read(&self.ledger_path(), 8 * 1024 * 1024).unwrap()
    }
    fn batches(&self) -> Vec<(String, String)> {
        self.ledger()["batches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                (
                    b["batch"].as_str().unwrap().to_owned(),
                    b["state"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
    /// Every CLI call, one argv per entry.
    fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.stub.path().join("argv.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                line.split('\u{1f}')
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .collect()
    }
    fn key_calls(&self) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with(&["shell".to_owned(), "keys".to_owned()]))
            .collect()
    }
}

/// Logs each call (arguments separated by U+001F), reports one live shell until
/// a `dead` file exists, prints `output.json` for `shell output`, and answers
/// `shell keys` as `mode` says.
fn stub_cli(dir: &Path, shell: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let cli = dir.join("fake-riwork");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\n\
             d='{dir}'\n\
             for a in \"$@\"; do printf '%s\\037' \"$a\"; done >> \"$d/argv.log\"\n\
             printf '\\n' >> \"$d/argv.log\"\n\
             case \"$1 $2\" in\n\
             'shell list') if [ -e \"$d/dead\" ]; then echo '[]'; else printf '[{{\"id\":\"{shell}\",\"alive\":true}}]'; fi;;\n\
             'orchestrator list') echo '[]';;\n\
             'shell output') cat \"$d/output.json\";;\n\
             'shell keys')\n\
               [ -e \"$d/delay\" ] && sleep \"$(cat \"$d/delay\")\"\n\
               case \"$(cat \"$d/mode\" 2>/dev/null)\" in\n\
                 input_unavailable) echo 'riwork: input_unavailable: terminal input is disabled for this pane' >&2; exit 2;;\n\
                 not_found) echo 'riwork: not_found: unknown shell' >&2; exit 2;;\n\
                 invalid) echo 'riwork: invalid_request: bad item' >&2; exit 2;;\n\
                 not_sent) echo 'riwork: not_sent: tmux did not answer' >&2; exit 2;;\n\
                 old) printf \"riwork: Unknown shell command 'keys'\\nUsage...\\n\" >&2; exit 2;;\n\
                 partial) echo 'riwork: tmux send-keys did not finish within 5s' >&2; exit 2;;\n\
                 forged) printf 'tmux said\\nriwork: not_sent: x\\n' >&2; exit 2;;\n\
                 embedded) echo 'riwork: send failed near input_unavailable: not_sent: ' >&2; exit 2;;\n\
               esac;;\n\
             esac\n",
            dir = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}

fn code(response: &Value) -> &str {
    response["error"]["code"].as_str().unwrap_or("")
}
fn status(response: &Value) -> &str {
    assert_eq!(response["ok"], true, "{response}");
    response["result"]["status"].as_str().unwrap()
}
fn text_of(length: usize) -> String {
    "a".repeat(length)
}

#[tokio::test]
async fn keys_validation_matrix_fails_before_any_cli() {
    let f = Fixture::without_cli();
    let (shell, batch) = (new_uuid(), new_uuid());
    let enter = json!([{"key":"Enter"}]);
    let many = |n: usize| Value::Array(vec![json!({"key":"Up"}); n]);
    let mut bad: Vec<(&str, Value)> = vec![];
    let with_items = |items: Value| json!({"shell_id":shell,"batch":batch,"items":items});
    // Parameter shape.
    bad.push(("no params", json!({})));
    bad.push(("no shell_id", json!({"batch":batch,"items":enter})));
    bad.push(("no batch", json!({"shell_id":shell,"items":enter})));
    bad.push(("no items", json!({"shell_id":shell,"batch":batch})));
    bad.push((
        "unknown field",
        json!({"shell_id":shell,"batch":batch,"items":enter,"line":"x"}),
    ));
    for (name, value) in [
        ("short shell", json!("12345678")),
        ("upper shell", json!(shell.to_uppercase())),
        ("number shell", json!(7)),
        ("null shell", Value::Null),
    ] {
        bad.push((name, json!({"shell_id":value,"batch":batch,"items":enter})));
    }
    for (name, value) in [
        ("short batch", json!("12345678")),
        ("upper batch", json!(batch.to_uppercase())),
        ("braced batch", json!(format!("{{{batch}}}"))),
        ("simple batch", json!(batch.replace('-', ""))),
        ("number batch", json!(7)),
        ("null batch", Value::Null),
        ("empty batch", json!("")),
    ] {
        bad.push((name, json!({"shell_id":shell,"batch":value,"items":enter})));
    }
    // Item list.
    bad.push(("items object", with_items(json!({"key":"Enter"}))));
    bad.push(("items string", with_items(json!("Enter"))));
    bad.push(("items null", with_items(Value::Null)));
    bad.push(("items empty", with_items(json!([]))));
    bad.push(("65 items", with_items(many(65))));
    bad.push(("1000 items", with_items(many(1000))));
    // Item shape.
    for (name, item) in [
        ("string item", json!("Enter")),
        ("number item", json!(1)),
        ("null item", Value::Null),
        ("array item", json!(["key", "Enter"])),
        ("empty item", json!({})),
        ("both", json!({"text":"a","key":"Enter"})),
        ("text and null key", json!({"text":"a","key":null})),
        ("null text and key", json!({"text":null,"key":"Enter"})),
        ("both null", json!({"text":null,"key":null})),
        ("null text", json!({"text":null})),
        ("null key", json!({"key":null})),
        ("unknown field", json!({"text":"a","x":1})),
        ("unknown field on key", json!({"key":"Enter","x":1})),
        ("only unknown", json!({"line":"a"})),
        ("number text", json!({"text":5})),
        ("array text", json!({"text":["a"]})),
        ("object text", json!({"text":{"a":1}})),
        ("bool key", json!({"key":true})),
        ("number key", json!({"key":1})),
    ] {
        bad.push((name, with_items(json!([item]))));
    }
    // Text.
    let mut texts = vec![
        ("empty text", String::new()),
        ("4097 bytes", text_of(4097)),
        ("2049 two-byte chars", "é".repeat(2049)),
    ];
    for (name, control) in [
        ("newline", "a\nb"),
        ("return", "a\rb"),
        ("tab", "a\tb"),
        ("nul", "a\0b"),
        ("escape", "a\u{1b}b"),
        ("delete", "a\u{7f}b"),
        ("c1 control", "a\u{85}b"),
        ("c1 control end", "a\u{9f}"),
        ("line separator", "a\u{2028}b"),
        ("paragraph separator", "a\u{2029}b"),
        ("only newline", "\n"),
    ] {
        texts.push((name, control.to_owned()));
    }
    for (name, text) in &texts {
        bad.push((name, with_items(json!([{"text":text}]))));
    }
    bad.push((
        "4097 bytes across items",
        with_items(json!([{"text":text_of(2048)},{"text":text_of(2049)}])),
    ));
    bad.push((
        "text over the total with keys between",
        with_items(json!([{"text":text_of(4000)},{"key":"Enter"},{"text":text_of(97)}])),
    ));
    // Keys.
    for key in [
        "",
        "enter",
        "ENTER",
        "Return",
        "Space",
        "F1",
        "PgUp",
        "PageUp ",
        " Enter",
        "Enter\n",
        "C-",
        "C-A",
        "c-a",
        "C-1",
        "C-aa",
        "C--",
        "C-é",
        "M-a",
        "S-Tab",
        "BSpace",
        "DC",
        "PPage",
        "NPage",
        "Backspace2",
        "C-a\0",
        "ctrl-a",
    ] {
        bad.push(("bad key", with_items(json!([{"key":key}]))));
    }
    // A bad item anywhere spoils the batch.
    bad.push((
        "bad last item",
        with_items(json!([{"key":"Enter"},{"text":"ok"},{"key":"Nope"}])),
    ));
    for (name, params) in bad {
        let response = f.call(req("shell.keys", params)).await;
        assert_eq!(code(&response), "invalid_request", "{name}: {response}");
    }
    // Nothing was recorded, and no batch file exists.
    assert!(!f.ledger_path().exists());
}

#[tokio::test]
async fn keys_limits_are_inclusive_and_items_reach_the_cli_in_order_verbatim() {
    let f = Fixture::new();
    // The boundaries: 64 items, 4096 text bytes in all.
    let batch = new_uuid();
    let response = f
        .keys(&batch, Value::Array(vec![json!({"key":"Up"}); 64]))
        .await;
    assert_eq!(status(&response), "sent");
    let response = f.keys(&new_uuid(), json!([{"text":text_of(4096)}])).await;
    assert_eq!(status(&response), "sent");
    let response = f
        .keys(
            &new_uuid(),
            json!([{"text":text_of(2048)},{"key":"Tab"},{"text":"é".repeat(1024)}]),
        )
        .await;
    assert_eq!(status(&response), "sent");
    let response = f
        .keys(&new_uuid(), json!([{"text":"é"},{"key":"C-z"}]))
        .await;
    assert_eq!(status(&response), "sent");

    // Order and text are passed through untouched, after `--`.
    let items = json!([
        {"text":"ls -la;"},
        {"key":"Enter"},
        {"text":"--json"},
        {"text":"-t"},
        {"text":"\\;"},
        {"text":"héllo ✓ 日本語 🚀 "},
        {"key":"C-c"},
        {"key":"PageDown"},
        {"text":"t:k:Enter"},
    ]);
    let response = f.keys(&new_uuid(), items).await;
    assert_eq!(status(&response), "sent");
    let last = f.key_calls().pop().unwrap();
    let expected: Vec<String> = [
        "shell",
        "keys",
        &f.shell,
        "--",
        "t:ls -la;",
        "k:Enter",
        "t:--json",
        "t:-t",
        "t:\\;",
        "t:héllo ✓ 日本語 🚀 ",
        "k:C-c",
        "k:PageDown",
        "t:t:k:Enter",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    assert_eq!(last, expected);
    // Every allowed key name is accepted and encoded as given.
    let names: Vec<String> = [
        "Enter",
        "Tab",
        "BTab",
        "Escape",
        "Backspace",
        "Delete",
        "Up",
        "Down",
        "Left",
        "Right",
        "Home",
        "End",
        "PageUp",
        "PageDown",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .chain(('a'..='z').map(|c| format!("C-{c}")))
    .collect();
    for chunk in names.chunks(20) {
        let items: Vec<Value> = chunk.iter().map(|k| json!({"key":k})).collect();
        let response = f.keys(&new_uuid(), Value::Array(items)).await;
        assert_eq!(status(&response), "sent", "{chunk:?}");
        let argv = f.key_calls().pop().unwrap();
        let sent: Vec<String> = chunk.iter().map(|k| format!("k:{k}")).collect();
        assert_eq!(argv[4..], sent[..]);
    }
}

#[tokio::test]
async fn keys_batches_are_delivered_once_per_device_and_repeats_send_nothing() {
    let f = Fixture::new();
    let batch = new_uuid();
    let items = json!([{"text":"echo hi"},{"key":"Enter"}]);
    let first = f.keys(&batch, items.clone()).await;
    assert_eq!(status(&first), "sent");
    assert_eq!(
        first["result"],
        json!({"shell_id":f.shell,"batch":batch,"status":"sent"})
    );
    assert_eq!(f.batches(), [(batch.clone(), "sent".to_owned())]);
    assert_eq!(f.key_calls().len(), 1);

    // A retry has a new request id and the same batch: nothing is sent again,
    // and no CLI runs at all to say so.
    let calls_before = f.calls().len();
    for _ in 0..3 {
        let retry = f.keys(&batch, items.clone()).await;
        assert_eq!(
            retry["result"],
            json!({"shell_id":f.shell,"batch":batch,"status":"duplicate"}),
            "{retry}"
        );
    }
    assert_eq!(f.calls().len(), calls_before);
    assert_eq!(f.key_calls().len(), 1);
    // Dedupe is by batch alone: other contents under the same batch do not send.
    let other = f.keys(&batch, json!([{"key":"C-c"}])).await;
    assert_eq!(status(&other), "duplicate");
    // ... and it holds even after the shell has gone.
    f.set("dead", "");
    assert_eq!(status(&f.keys(&batch, items.clone()).await), "duplicate");
    std::fs::remove_file(f.stub.path().join("dead")).unwrap();

    // A crash between the pending record and the send leaves `pending`.
    let crashed = new_uuid();
    private_write(
        &f.ledger_path(),
        &json!({"batches":[{"batch":batch,"state":"sent"},{"batch":crashed,"state":"pending"}]}),
    )
    .unwrap();
    let calls_before = f.calls().len();
    for _ in 0..2 {
        let retry = f.keys(&crashed, items.clone()).await;
        assert_eq!(
            retry["result"],
            json!({"shell_id":f.shell,"batch":crashed,"status":"uncertain"}),
            "{retry}"
        );
    }
    assert_eq!(f.calls().len(), calls_before);
    assert_eq!(f.batches()[1], (crashed.clone(), "pending".to_owned()));
    // Later batches are unaffected by an uncertain one.
    assert_eq!(status(&f.keys(&new_uuid(), items.clone()).await), "sent");

    // The ledger is per device.
    let second = pair(&f.rpc.storage, f.stub.path(), "tablet");
    let response = f
        .rpc
        .handle(&second, keys_req(&f.shell, &batch, items.clone()))
        .await
        .unwrap();
    assert_eq!(status(&response), "sent");
    assert!(
        f.rpc
            .storage
            .dir
            .join(format!("keys-{second}.json"))
            .exists()
    );
    // A revoked device is cut off like any other.
    f.rpc.storage.revoke(&f.device).unwrap();
    assert!(
        f.rpc
            .handle(&f.device, keys_req(&f.shell, &new_uuid(), items))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn keys_errors_before_typing_forget_the_batch_and_any_other_keeps_it() {
    let f = Fixture::new();
    let items = json!([{"text":"x"},{"key":"Enter"}]);

    // Definitely nothing typed: the batch is forgotten, so a retry can send it.
    for (mode, expected) in [
        ("input_unavailable", "input_unavailable"),
        ("not_found", "not_found"),
        ("invalid", "invalid_request"),
        ("not_sent", "cli_error"),
        ("old", "cli_error"),
    ] {
        f.set("mode", mode);
        let batch = new_uuid();
        let response = f.keys(&batch, items.clone()).await;
        assert_eq!(code(&response), expected, "{mode}: {response}");
        assert!(
            f.batches().iter().all(|(b, _)| *b != batch),
            "{mode} left {batch} behind"
        );
        f.set("mode", "");
        assert_eq!(
            status(&f.keys(&batch, items.clone()).await),
            "sent",
            "{mode}"
        );
        assert_eq!(f.batches().last().unwrap(), &(batch, "sent".to_owned()));
    }
    // The message is the CLI's, without its token or a second line.
    f.set("mode", "input_unavailable");
    let response = f.keys(&new_uuid(), items.clone()).await;
    assert_eq!(
        response["error"]["message"],
        "terminal input is disabled for this pane"
    );

    // Anything else may have typed part of the batch: it stays pending, the
    // error is a cli_error, and a retry is answered `uncertain` without a CLI.
    for mode in ["partial", "forged", "embedded"] {
        f.set("mode", mode);
        let batch = new_uuid();
        let response = f.keys(&batch, items.clone()).await;
        assert_eq!(code(&response), "cli_error", "{mode}: {response}");
        assert_eq!(
            f.batches().last().unwrap(),
            &(batch.clone(), "pending".to_owned())
        );
        f.set("mode", "");
        let calls = f.key_calls().len();
        let retry = f.keys(&batch, items.clone()).await;
        assert_eq!(status(&retry), "uncertain", "{mode}");
        assert_eq!(f.key_calls().len(), calls);
    }

    // A CLI that cannot even start is not a typed batch either way: the shell
    // lookup fails first, so nothing is recorded.
    let batches = f.batches().len();
    let broken = Rpc::new("/nonexistent/riwork".into(), f.rpc.storage.clone());
    let response = broken
        .handle(&f.device, keys_req(&f.shell, &new_uuid(), items.clone()))
        .await
        .unwrap();
    assert_eq!(code(&response), "cli_error", "{response}");
    assert_eq!(f.batches().len(), batches);

    // An unknown or dead shell is not_found and leaves the ledger alone.
    f.set("mode", "");
    let batch = new_uuid();
    let unknown = f.call(keys_req(&new_uuid(), &batch, items.clone())).await;
    assert_eq!(code(&unknown), "not_found", "{unknown}");
    f.set("dead", "");
    let dead = f.keys(&batch, items.clone()).await;
    assert_eq!(code(&dead), "not_found", "{dead}");
    assert_eq!(f.batches().len(), batches);
    assert!(f.key_calls().iter().all(|c| c[2] == f.shell));
    let keys_calls = f.key_calls().len();
    std::fs::remove_file(f.stub.path().join("dead")).unwrap();
    assert_eq!(status(&f.keys(&batch, items).await), "sent");
    assert_eq!(f.key_calls().len(), keys_calls + 1);
}

#[cfg(unix)]
#[tokio::test]
async fn keys_ledger_is_private_atomic_bounded_and_separate_from_input_outcomes() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let items = json!([{"key":"Enter"}]);
    assert_eq!(status(&f.keys(&new_uuid(), items.clone()).await), "sent");
    let mode = std::fs::metadata(f.ledger_path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let leftovers: Vec<_> = std::fs::read_dir(&f.rpc.storage.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".remote-") || n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // The input outcome ledger is not created, and not needed.
    assert!(
        !f.rpc
            .storage
            .dir
            .join(format!("outcomes-{}.json", f.device))
            .exists()
    );

    // Bounded: the most recent 4096 batches stay, the oldest go first.
    let filler: Vec<String> = (0..KEYS_LEDGER_MAX).map(|_| new_uuid()).collect();
    let records = |ids: &[String]| -> Value {
        json!({"batches":ids.iter().map(|b| json!({"batch":b,"state":"sent"})).collect::<Vec<_>>()})
    };
    private_write(&f.ledger_path(), &records(&filler)).unwrap();
    let newest = new_uuid();
    assert_eq!(status(&f.keys(&newest, items.clone()).await), "sent");
    let after = f.batches();
    assert_eq!(after.len(), KEYS_LEDGER_MAX);
    assert_eq!(after.last().unwrap(), &(newest.clone(), "sent".to_owned()));
    assert_eq!(after[0].0, filler[1], "only the oldest was pruned");
    assert!(
        after
            .iter()
            .all(|(_, state)| state == "sent" || state == "pending")
    );
    // A retained batch is still a duplicate; the pruned one is new again.
    assert_eq!(
        status(&f.keys(&filler[1], items.clone()).await),
        "duplicate"
    );
    assert_eq!(status(&f.keys(&filler[0], items.clone()).await), "sent");
    let after = f.batches();
    assert_eq!(after.len(), KEYS_LEDGER_MAX);
    assert_eq!(after[0].0, filler[2]);
    assert_eq!(after.last().unwrap().0, filler[0]);
    // A ledger above the bound (tampered) shrinks back to it.
    let over: Vec<String> = (0..KEYS_LEDGER_MAX + 50).map(|_| new_uuid()).collect();
    private_write(&f.ledger_path(), &records(&over)).unwrap();
    assert_eq!(status(&f.keys(&new_uuid(), items.clone()).await), "sent");
    let after = f.batches();
    assert_eq!(after.len(), KEYS_LEDGER_MAX);
    assert_eq!(after[0].0, over[51]);
    // Pending records are pruned by age like any other.
    let mut aged = filler.clone();
    aged.truncate(KEYS_LEDGER_MAX);
    let mut ledger = records(&aged);
    ledger["batches"][0]["state"] = json!("pending");
    private_write(&f.ledger_path(), &ledger).unwrap();
    assert_eq!(status(&f.keys(&aged[0], items.clone()).await), "uncertain");
    assert_eq!(status(&f.keys(&new_uuid(), items.clone()).await), "sent");
    assert_eq!(status(&f.keys(&aged[0], items.clone()).await), "sent");

    // A ledger that others could read or write is refused, and nothing is typed.
    let calls = f.key_calls().len();
    std::fs::set_permissions(f.ledger_path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    let response = f.keys(&new_uuid(), items.clone()).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    assert_eq!(f.key_calls().len(), calls);
    // So is one that does not parse.
    std::fs::set_permissions(f.ledger_path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(f.ledger_path(), "{\"batches\":[{\"batch\":1}]}").unwrap();
    let response = f.keys(&new_uuid(), items).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    assert_eq!(f.key_calls().len(), calls);
}

#[tokio::test]
async fn keys_coexist_with_the_input_ledger_and_its_request_id_guard() {
    let f = Fixture::new();
    let input = req("shell.input", json!({"shell_id":f.shell,"line":"echo one"}));
    assert_eq!(status(&f.call(input.clone()).await), "sent");
    let outcomes = f
        .rpc
        .storage
        .dir
        .join(format!("outcomes-{}.json", f.device));
    let recorded = std::fs::read(&outcomes).unwrap();

    // With an input ledger on disk, keys still go through and leave it alone.
    let items = json!([{"text":"two"},{"key":"Enter"}]);
    let batch = new_uuid();
    assert_eq!(status(&f.keys(&batch, items.clone()).await), "sent");
    assert_eq!(status(&f.keys(&batch, items.clone()).await), "duplicate");
    assert_eq!(std::fs::read(&outcomes).unwrap(), recorded);
    // And input still dedupes by its request id, unaffected by batches.
    assert_eq!(status(&f.call(input.clone()).await), "sent");
    assert_eq!(
        f.calls()
            .iter()
            .filter(|c| c.starts_with(&["shell".to_owned(), "send".to_owned()]))
            .count(),
        1
    );

    // A request id that input has used stays used, for keys as for any method.
    let mut reused = keys_req(&f.shell, &new_uuid(), items.clone());
    reused["id"] = input["id"].clone();
    let response = f.call(reused.clone()).await;
    assert_eq!(code(&response), "request_conflict", "{response}");
    assert_eq!(f.key_calls().len(), 1);
    // The refusal did not consume the batch: a fresh request id sends it.
    let batch = reused["params"]["batch"].as_str().unwrap().to_owned();
    assert_eq!(status(&f.keys(&batch, items).await), "sent");
    // Request ids of keys are not recorded anywhere: reusing one is harmless.
    let again = keys_req(&f.shell, &new_uuid(), json!([{"key":"Enter"}]));
    assert_eq!(status(&f.call(again.clone()).await), "sent");
    let mut replay = again;
    replay["params"]["batch"] = json!(new_uuid());
    assert_eq!(status(&f.call(replay).await), "sent");
}

#[tokio::test]
async fn a_retry_that_overlaps_its_first_attempt_waits_and_is_a_duplicate() {
    let f = Fixture::new();
    f.set("delay", "0.4");
    let batch = new_uuid();
    let items = json!([{"text":"once"}]);
    let (first, second) =
        tokio::join!(f.keys(&batch, items.clone()), f.keys(&batch, items.clone()));
    let mut got = [status(&first), status(&second)];
    got.sort_unstable();
    assert_eq!(got, ["duplicate", "sent"]);
    assert_eq!(f.key_calls().len(), 1);
    assert_eq!(f.batches(), [(batch, "sent".to_owned())]);
}

#[tokio::test]
async fn shell_output_passes_screen_fields_through_and_omits_what_it_cannot_trust() {
    let f = Fixture::new();
    let output = |text: &str, extra: Value| -> Value {
        let mut value = json!({"id":f.shell,"output":text});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        value
    };
    let ask = |cli: Value| {
        f.set("output.json", &cli.to_string());
        f.call(req("shell.output", json!({"shell_id":f.shell})))
    };
    let screen = json!({"cursor":{"x":1,"y":1},"rows":2,"cols":80,"in_mode":false});
    let response = ask(output("history\nab\ncd\n", screen.clone())).await;
    assert_eq!(
        response["result"],
        json!({"shell_id":f.shell,"output":"history\nab\ncd\n",
               "cursor":{"x":1,"y":1},"rows":2,"cols":80,"in_mode":false})
    );
    let response = ask(output(
        "ab\ncd\n",
        json!({"cursor":{"x":0,"y":0},"rows":2,"cols":9,"in_mode":true}),
    ))
    .await;
    assert_eq!(response["result"]["in_mode"], true);
    assert_eq!(response["result"]["cols"], 9);
    // An older CLI, or one that could not read the pane: the result is as before.
    let plain = json!({"shell_id":f.shell,"output":"ab\ncd\n"});
    assert_eq!(ask(output("ab\ncd\n", json!({}))).await["result"], plain);
    for broken in [
        json!({"cursor":{"x":0,"y":2},"rows":2,"cols":80,"in_mode":false}),
        json!({"cursor":{"x":0,"y":0},"rows":3,"cols":80,"in_mode":false}),
        json!({"cursor":{"x":0,"y":0},"rows":0,"cols":80,"in_mode":false}),
        json!({"cursor":{"x":0,"y":0},"rows":2,"cols":0,"in_mode":false}),
        json!({"cursor":{"x":-1,"y":0},"rows":2,"cols":80,"in_mode":false}),
        json!({"cursor":{"x":1.5,"y":0},"rows":2,"cols":80,"in_mode":false}),
        json!({"cursor":{"x":4294967296u64,"y":0},"rows":2,"cols":80,"in_mode":false}),
        json!({"cursor":{"x":"1","y":0},"rows":2,"cols":80,"in_mode":false}),
        json!({"cursor":{"x":0},"rows":2,"cols":80,"in_mode":false}),
        json!({"cursor":[0,0],"rows":2,"cols":80,"in_mode":false}),
        json!({"cursor":null,"rows":2,"cols":80,"in_mode":false}),
        json!({"cursor":{"x":0,"y":0},"rows":"2","cols":80,"in_mode":false}),
        json!({"cursor":{"x":0,"y":0},"rows":2,"cols":80,"in_mode":"no"}),
        json!({"cursor":{"x":0,"y":0},"rows":2,"cols":80}),
        json!({"cursor":{"x":0,"y":0},"rows":2,"in_mode":false}),
        json!({"rows":2,"cols":80,"in_mode":false}),
        json!({"cursor":{"x":0,"y":0},"cols":80,"in_mode":false}),
    ] {
        let response = ask(output("ab\ncd\n", broken.clone())).await;
        assert_eq!(response["result"], plain, "{broken}");
    }
    // Only the four fields travel: nothing else the CLI printed is forwarded.
    let response = ask(output(
        "ab\ncd\n",
        json!({"cursor":{"x":0,"y":0,"z":1},"rows":2,"cols":80,"in_mode":false,"secret":"x"}),
    ))
    .await;
    let mut keys: Vec<&str> = response["result"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["cols", "cursor", "in_mode", "output", "rows", "shell_id"]
    );
    assert_eq!(response["result"]["cursor"], json!({"x":0,"y":0}));
    // The CLI is asked exactly as before (its JSON now carries the fields).
    let call = f
        .calls()
        .into_iter()
        .rev()
        .find(|c| c[1] == "output")
        .unwrap();
    assert_eq!(
        call,
        ["shell", "output", &f.shell, "--lines", "200", "--json"]
    );
}

/// The same matrix through the RPC and through the desktop CLI's own parser:
/// they must accept and refuse exactly the same batches.
#[tokio::test]
async fn the_rpc_validator_agrees_with_the_cli_validator() {
    let f = Fixture::new();
    let mut batches: Vec<Vec<(&str, String)>> = vec![];
    let single = |kind: &'static str, value: String| vec![(kind, value)];
    let texts = [
        "",
        "a",
        " ",
        "select 1;",
        "\\;",
        "héllo ✓ 日本語 🚀",
        "a\nb",
        "a\rb",
        "a\tb",
        "a\0b",
        "a\u{1b}b",
        "a\u{7f}b",
        "a\u{85}b",
        "a\u{9f}b",
        "a\u{2028}b",
        "a\u{2029}b",
        "\u{a0}\u{200b}\u{feff}",
        "t:x",
        "k:Enter",
        "--",
        "-l",
    ];
    for text in texts {
        batches.push(single("text", text.to_owned()));
    }
    for size in [4095, 4096, 4097] {
        batches.push(single("text", text_of(size)));
    }
    for size in [2047, 2048, 2049] {
        batches.push(single("text", "é".repeat(size)));
    }
    let keys = [
        "Enter",
        "Tab",
        "BTab",
        "Escape",
        "Backspace",
        "Delete",
        "Up",
        "Down",
        "Left",
        "Right",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "C-a",
        "C-m",
        "C-z",
        "",
        "enter",
        "Return",
        "F1",
        "C-A",
        "C-1",
        "C-",
        "C-aa",
        "C-é",
        "M-a",
        "BSpace",
        "DC",
        "PPage",
        "NPage",
        " Enter",
        "Enter ",
        "Enter\n",
        "C-a\0",
    ];
    for key in keys {
        batches.push(single("key", key.to_owned()));
    }
    // Multi-item batches around the limits.
    for count in [1, 63, 64, 65] {
        batches.push((0..count).map(|_| ("key", "Up".to_owned())).collect());
    }
    for (first, second) in [(2048, 2048), (2048, 2049), (4096, 1), (1, 4096), (4095, 1)] {
        batches.push(vec![
            ("text", text_of(first)),
            ("key", "Enter".into()),
            ("text", text_of(second)),
        ]);
    }
    let mut accepted = 0;
    let mut refused = 0;
    for batch in batches {
        let argv: Vec<String> = batch
            .iter()
            .map(|(kind, value)| format!("{}:{value}", if *kind == "text" { "t" } else { "k" }))
            .collect();
        let items: Vec<Value> = batch
            .iter()
            .map(|(kind, value)| {
                let mut item = serde_json::Map::new();
                item.insert((*kind).to_owned(), json!(value));
                Value::Object(item)
            })
            .collect();
        let cli = session_keys::parse_items(&argv);
        let response = f.keys(&new_uuid(), Value::Array(items)).await;
        let rpc_ok = code(&response) != "invalid_request";
        assert_eq!(
            rpc_ok,
            cli.is_ok(),
            "RPC and CLI disagree on {:?}: rpc {response}, cli {cli:?}",
            argv.iter()
                .map(|a| a.chars().take(30).collect::<String>())
                .collect::<Vec<_>>()
        );
        if rpc_ok {
            accepted += 1;
        } else {
            refused += 1;
        }
    }
    assert!(
        accepted > 30 && refused > 30,
        "{accepted} accepted, {refused} refused"
    );
}

/// The production key module, compiled without GPUI, against a real private
/// tmux: text ending in `;` and `\;` arrives verbatim, keys arrive as bytes.
#[cfg(unix)]
#[test]
#[ignore = "slow: drives a real private tmux server"]
fn the_production_key_module_types_into_a_real_tmux() {
    use session_keys::{Item, Key};
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    let socket = format!("riwork-keys-test-{}", new_uuid());
    let tmux = |args: &[&str]| -> Result<String, String> {
        let out = Command::new("tmux")
            .args(["-L", &socket, "-f", "/dev/null"])
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    };
    if tmux(&["-V"]).is_err() {
        return; // no tmux here
    }
    struct Kill<'a>(&'a dyn Fn(&[&str]) -> Result<String, String>);
    impl Drop for Kill<'_> {
        fn drop(&mut self) {
            let _ = (self.0)(&["kill-server"]);
        }
    }
    let _kill = Kill(&tmux);
    let home = tempfile::tempdir().unwrap();
    let received = home.path().join("received");
    let id = new_uuid();
    tmux(&[
        "new-session",
        "-d",
        "-s",
        &id,
        "-x",
        "80",
        "-y",
        "24",
        &format!(
            "sh -c 'stty raw -echo cs8; : > {ready}; exec cat >> {received}'",
            ready = home.path().join("ready").display(),
            received = received.display()
        ),
    ])
    .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while !home.path().join("ready").exists() {
        assert!(Instant::now() < end, "pane never became ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    let wait = |length: usize| {
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            let bytes = std::fs::read(&received).unwrap_or_default();
            if bytes.len() >= length || Instant::now() > end {
                return bytes;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let t = |args: &[&str]| tmux(args);
    let mut expected = Vec::new();
    for text in [
        "select 1;",
        "\\;",
        ";",
        "a\\",
        "héllo ✓ 🚀;",
        "-b -t #{pane_id};",
    ] {
        session_keys::send(home.path(), &id, &[Item::Text(text.into())], &t).unwrap();
        expected.extend_from_slice(text.as_bytes());
        assert_eq!(wait(expected.len()), expected, "{text}");
    }
    let batch = [
        Item::Text("ls;".into()),
        Item::Key(Key::Enter),
        Item::Key(Key::Ctrl(b'c')),
        Item::Key(Key::Up),
    ];
    session_keys::send(home.path(), &id, &batch, &t).unwrap();
    expected.extend_from_slice(b"ls;\r\x03\x1b[A");
    assert_eq!(wait(expected.len()), expected);
    // Input off is refused with its token, and nothing more arrives.
    tmux(&["select-pane", "-d", "-t", &format!("{id}:0.0")]).unwrap();
    let error = session_keys::send(home.path(), &id, &[Item::Key(Key::Enter)], &t).unwrap_err();
    assert!(
        error.starts_with(session_keys::INPUT_UNAVAILABLE),
        "{error}"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(std::fs::read(&received).unwrap(), expected);
}
