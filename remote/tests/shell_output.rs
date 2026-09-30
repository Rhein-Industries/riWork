//! The additive `styled`, `if_changed` and `wait_ms` parameters of
//! `shell.output`, its `hash` and its `unchanged` result, against a stub CLI
//! that records its argv and prints whatever `output.json` holds.
use riwork_remote::{config::Storage, rpc::Rpc};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn req(method: &str, params: Value) -> Value {
    json!({"v":1,"type":"request","id":new_uuid(),"method":method,"params":params})
}

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
        let cli = stub_cli(stub.path(), &shell);
        Self {
            rpc: Rpc::new(cli, storage),
            _storage_dir: storage_dir,
            stub,
            device,
            shell,
        }
    }
    /// Every request must be answered before a CLI would run.
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
    async fn output(&self, params: Value) -> Value {
        let mut params = params;
        params["shell_id"] = json!(self.shell);
        self.call(req("shell.output", params)).await
    }
    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.stub.path().join(name), value).unwrap();
    }
    fn cli_says(&self, value: Value) {
        self.set("output.json", &value.to_string());
    }
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
    /// The argv of the last `shell output` call.
    fn output_call(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .rev()
            .find(|c| c.get(1).is_some_and(|word| word == "output"))
            .expect("the CLI was asked for output")
    }
}

/// Logs each call (arguments separated by U+001F), reports one live shell,
/// prints `output.json` for `shell output` and, after `send-delay` seconds if
/// that file exists, does nothing for `shell send`. `old` in `mode` makes
/// `shell output` refuse like a CLI from before this feature.
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
             'shell list') printf '[{{\"id\":\"{shell}\",\"alive\":true}}]';;\n\
             'orchestrator list') echo '[]';;\n\
             'shell output')\n\
               if [ \"$(cat \"$d/mode\" 2>/dev/null)\" = old ]; then\n\
                 echo 'riwork: Usage: riwork shell output ID [--lines N]' >&2; exit 2\n\
               fi\n\
               cat \"$d/output.json\";;\n\
             'shell send') [ -e \"$d/send-delay\" ] && sleep \"$(cat \"$d/send-delay\")\";;\n\
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

const HASH: &str = "0123456789abcdef";

#[tokio::test]
async fn the_new_parameters_are_validated_before_any_cli_runs() {
    let f = Fixture::without_cli();
    let long = "h".repeat(65);
    let bad = [
        json!({"styled":"yes"}),
        json!({"styled":1}),
        json!({"styled":[]}),
        json!({"wait_ms":-1,"if_changed":HASH}),
        json!({"wait_ms":10001,"if_changed":HASH}),
        json!({"wait_ms":1.5,"if_changed":HASH}),
        json!({"wait_ms":"500","if_changed":HASH}),
        json!({"wait_ms":true,"if_changed":HASH}),
        json!({"wait_ms":u64::MAX,"if_changed":HASH}),
        // Out of range is refused even without something to wait for.
        json!({"wait_ms":10001}),
        json!({"if_changed":""}),
        json!({"if_changed":long}),
        json!({"if_changed":"has space"}),
        json!({"if_changed":"tab\there"}),
        json!({"if_changed":"line\nbreak"}),
        json!({"if_changed":"esc\u{1b}"}),
        json!({"if_changed":"caf\u{e9}"}),
        json!({"if_changed":7}),
        json!({"if_changed":["a"]}),
        json!({"unknown":true}),
        json!({"lines":0,"styled":true}),
        json!({"lines":2001,"if_changed":HASH}),
    ];
    for params in bad {
        let response = f.output(params.clone()).await;
        assert_eq!(code(&response), "invalid_request", "{params}: {response}");
    }
    assert!(!f.stub.path().join("argv.log").exists());
}

#[tokio::test]
async fn the_bounds_of_the_new_parameters_are_inclusive_and_null_means_absent() {
    let f = Fixture::new();
    f.cli_says(json!({"id":f.shell,"output":"x\n","hash":HASH}));
    let sixty_four = "h".repeat(64);
    for params in [
        json!({"styled":true,"if_changed":HASH,"wait_ms":0}),
        json!({"if_changed":HASH,"wait_ms":10000}),
        json!({"if_changed":sixty_four}),
        json!({"styled":false,"if_changed":null,"wait_ms":null}),
        json!({"wait_ms":10000}),
        json!({"wait_ms":0}),
    ] {
        let response = f.output(params.clone()).await;
        assert_eq!(response["ok"], true, "{params}: {response}");
    }
}

#[tokio::test]
async fn a_plain_request_is_asked_of_the_cli_as_before_and_answered_with_a_hash() {
    let f = Fixture::new();
    f.cli_says(json!({
        "id": f.shell, "output": "ab\ncd\n", "hash": HASH,
        "cursor": {"x":1,"y":1}, "rows": 2, "cols": 80, "in_mode": false
    }));
    let response = f.output(json!({})).await;
    assert_eq!(
        response["result"],
        json!({"shell_id":f.shell,"output":"ab\ncd\n","cursor":{"x":1,"y":1},
               "rows":2,"cols":80,"in_mode":false,"hash":HASH})
    );
    assert_eq!(
        f.output_call(),
        ["shell", "output", &f.shell, "--lines", "200", "--json"],
        "no new flag unless the request used one"
    );
    // A wait without a hash to compare has nothing to wait for: not passed on.
    f.output(json!({"wait_ms":5000})).await;
    assert_eq!(
        f.output_call(),
        ["shell", "output", &f.shell, "--lines", "200", "--json"]
    );
    // A CLI without hashes (an older one) gives the result of before.
    f.cli_says(json!({"id":f.shell,"output":"ab\n"}));
    let response = f.output(json!({})).await;
    assert_eq!(
        response["result"],
        json!({"shell_id":f.shell,"output":"ab\n"})
    );
    // A hash that is not a plausible hash is not forwarded.
    for bad in ["", "two words", "\u{e9}", &"x".repeat(65)] {
        f.cli_says(json!({"id":f.shell,"output":"ab\n","hash":bad}));
        let response = f.output(json!({})).await;
        assert_eq!(response["result"].get("hash"), None, "{bad:?}");
    }
    f.cli_says(json!({"id":f.shell,"output":"ab\n","hash":7}));
    assert_eq!(f.output(json!({})).await["result"].get("hash"), None);
}

#[tokio::test]
async fn styled_if_changed_and_wait_reach_the_cli_as_arguments() {
    let f = Fixture::new();
    f.cli_says(json!({"id":f.shell,"output":"x\n","hash":HASH}));
    f.output(json!({"lines":50,"styled":true})).await;
    assert_eq!(
        f.output_call(),
        [
            "shell", "output", &f.shell, "--lines", "50", "--styled", "--json"
        ]
    );
    f.output(json!({"lines":50,"if_changed":HASH,"wait_ms":2500}))
        .await;
    assert_eq!(
        f.output_call(),
        [
            "shell",
            "output",
            &f.shell,
            "--lines",
            "50",
            &format!("--if-changed={HASH}"),
            "--wait-ms",
            "2500",
            "--json"
        ]
    );
    f.output(json!({"styled":true,"if_changed":HASH})).await;
    assert_eq!(
        f.output_call(),
        [
            "shell",
            "output",
            &f.shell,
            "--lines",
            "200",
            "--styled",
            &format!("--if-changed={HASH}"),
            "--wait-ms",
            "0",
            "--json"
        ]
    );
    // A hash that looks like an option is still one argument of its own option.
    for odd in ["--json", "--lines", "-x", "--wait-ms"] {
        f.output(json!({"if_changed":odd,"wait_ms":1})).await;
        let call = f.output_call();
        assert!(call.contains(&format!("--if-changed={odd}")), "{call:?}");
        assert_eq!(call.len(), 9, "{call:?}");
    }
}

#[tokio::test]
async fn an_unchanged_answer_has_no_output_and_the_same_hash() {
    let f = Fixture::new();
    f.cli_says(json!({"id":f.shell,"unchanged":true,"hash":HASH}));
    let response = f.output(json!({"if_changed":HASH,"wait_ms":100})).await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["result"],
        json!({"shell_id":f.shell,"unchanged":true,"hash":HASH})
    );
    // Nothing else the CLI printed travels with it.
    f.cli_says(
        json!({"id":f.shell,"unchanged":true,"hash":HASH,"output":"leak","cursor":{"x":0,"y":0}}),
    );
    let response = f.output(json!({"if_changed":HASH})).await;
    assert_eq!(
        response["result"],
        json!({"shell_id":f.shell,"unchanged":true,"hash":HASH})
    );
    // A change is answered in full, with the new hash.
    f.cli_says(json!({"id":f.shell,"output":"new\n","hash":"fedcba9876543210"}));
    let response = f.output(json!({"if_changed":HASH,"wait_ms":100})).await;
    assert_eq!(
        response["result"],
        json!({"shell_id":f.shell,"output":"new\n","hash":"fedcba9876543210"})
    );
}

#[tokio::test]
async fn an_unchanged_answer_nobody_asked_for_is_a_cli_error() {
    let f = Fixture::new();
    for (params, cli) in [
        // No hash was sent.
        (json!({}), json!({"unchanged":true,"hash":HASH})),
        (json!({"wait_ms":10}), json!({"unchanged":true,"hash":HASH})),
        // Another hash, or none, or a bad one, came back.
        (
            json!({"if_changed":HASH}),
            json!({"unchanged":true,"hash":"fedcba9876543210"}),
        ),
        (json!({"if_changed":HASH}), json!({"unchanged":true})),
        (
            json!({"if_changed":HASH}),
            json!({"unchanged":true,"hash":"has space"}),
        ),
    ] {
        f.cli_says(cli.clone());
        let response = f.output(params.clone()).await;
        assert_eq!(code(&response), "cli_error", "{params} {cli}: {response}");
    }
    // `unchanged` that is not true is just a field: the content is what counts.
    f.cli_says(json!({"id":f.shell,"unchanged":false,"output":"x\n","hash":HASH}));
    let response = f.output(json!({"if_changed":"0000"})).await;
    assert_eq!(response["result"]["output"], "x\n");
}

#[tokio::test]
async fn styled_output_carries_sgr_and_nothing_else_or_the_request_fails() {
    let f = Fixture::new();
    let sgr = "\u{1b}[1;31mred\u{1b}[0m \u{1b}[38;2;1;2;3mtrue\u{1b}[0m\n\u{1b}[0mnext\n";
    f.cli_says(json!({"id":f.shell,"output":sgr,"hash":HASH}));
    let response = f.output(json!({"styled":true})).await;
    assert_eq!(response["result"]["output"], sgr, "{response}");
    for dirty in [
        "a\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\\n",
        "a\u{1b}[2Jb\n",
        "a\u{1b}[?25lb\n",
        "a\u{1b}(0b\n",
        "a\u{e}b\n",
        "a\rb\n",
        "a\u{1b}[31\n",
        "a\u{1b}\n",
    ] {
        f.cli_says(json!({"id":f.shell,"output":dirty,"hash":HASH}));
        let response = f.output(json!({"styled":true})).await;
        assert_eq!(code(&response), "cli_error", "{dirty:?}: {response}");
        // Not asked for styled text, the request is not judged as such.
        let response = f.output(json!({})).await;
        assert_eq!(response["ok"], true, "{dirty:?}: {response}");
        assert_eq!(response["result"]["output"], dirty);
    }
}

#[tokio::test]
async fn a_cli_from_before_this_feature_is_named_when_the_new_flags_fail() {
    let f = Fixture::new();
    f.set("mode", "old");
    let response = f.output(json!({"styled":true})).await;
    assert_eq!(code(&response), "cli_error");
    let message = response["error"]["message"].as_str().unwrap();
    assert!(message.contains("update RiWork"), "{message}");
    let response = f.output(json!({"if_changed":HASH,"wait_ms":10})).await;
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("update RiWork")
    );
    // A plain request would not have used a new flag: its error is the CLI's.
    let response = f.output(json!({})).await;
    let message = response["error"]["message"].as_str().unwrap();
    assert!(
        !message.contains("update RiWork") && message.contains("Usage"),
        "{message}"
    );
}

#[tokio::test]
async fn errors_are_the_same_as_before() {
    let f = Fixture::new();
    // An unknown shell is not_found, whatever else the request says.
    let response = f
        .call(req(
            "shell.output",
            json!({"shell_id":new_uuid(),"styled":true,"if_changed":HASH,"wait_ms":5}),
        ))
        .await;
    assert_eq!(code(&response), "not_found", "{response}");
    let response = f
        .call(req(
            "shell.output",
            json!({"shell_id":"nope","styled":true}),
        ))
        .await;
    assert_eq!(code(&response), "invalid_request");
    f.set("output.json", "not json");
    assert_eq!(code(&f.output(json!({"styled":true})).await), "cli_error");
    f.cli_says(json!({"id":f.shell}));
    assert_eq!(code(&f.output(json!({"styled":true})).await), "cli_error");
}

/// The input file lock is a try-lock; requests of one device now overlap, so
/// two inputs must queue on each other rather than fail.
#[tokio::test]
async fn overlapping_inputs_of_one_device_wait_for_each_other() {
    let f = Fixture::new();
    f.set("send-delay", "0.4");
    let input = |line: &str| req("shell.input", json!({"shell_id":f.shell,"line":line}));
    let (a, b, c) = tokio::join!(
        f.call(input("one")),
        f.call(input("two")),
        f.call(input("three"))
    );
    for response in [&a, &b, &c] {
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["result"]["status"], "sent");
    }
    let sends = f.calls().into_iter().filter(|c| c[1] == "send").count();
    assert_eq!(sends, 3);
}
