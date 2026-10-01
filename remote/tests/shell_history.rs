//! `shell.history`: validation before any CLI runs, the argv it turns into,
//! what it accepts back from the CLI, and its errors, against a stub CLI that
//! records its argv and prints whatever `history.json` holds.
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
    async fn history(&self, params: Value) -> Value {
        let mut params = params;
        params["shell_id"] = json!(self.shell);
        self.call(req("shell.history", params)).await
    }
    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.stub.path().join(name), value).unwrap();
    }
    fn cli_says(&self, value: Value) {
        self.set("history.json", &value.to_string());
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
    /// The argv of the last `shell history` call.
    fn history_call(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .rev()
            .find(|c| c.get(1).is_some_and(|word| word == "history"))
            .expect("the CLI was asked for history")
    }
    fn history_calls(&self) -> usize {
        self.calls()
            .iter()
            .filter(|c| c.get(1).is_some_and(|word| word == "history"))
            .count()
    }
}

/// Logs each call (arguments separated by U+001F), reports one shell (live,
/// or dead if the file `dead` exists) and prints `history.json` for `shell
/// history`. `old` in `mode` makes `shell history` fail like a CLI from
/// before this feature, and `fail` like one whose tmux call failed.
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
             'shell list')\n\
               if [ -e \"$d/dead\" ]; then alive=false; else alive=true; fi\n\
               printf '[{{\"id\":\"{shell}\",\"alive\":%s}}]' \"$alive\";;\n\
             'orchestrator list') echo '[]';;\n\
             'shell history')\n\
               case \"$(cat \"$d/mode\" 2>/dev/null)\" in\n\
                 old) echo \"riwork: Unknown shell command 'history'\" >&2; exit 2;;\n\
                 fail) echo 'riwork: tmux: no server running' >&2; exit 2;;\n\
               esac\n\
               cat \"$d/history.json\";;\n\
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

/// A page the CLI could have printed.
fn page(shell: &str, output: &str, count: u32, size: u32, complete: bool) -> Value {
    json!({"id":shell,"output":output,"line_count":count,"history_size":size,"complete":complete})
}

#[tokio::test]
async fn parameters_are_validated_before_any_cli_runs() {
    let f = Fixture::without_cli();
    let shell = f.shell.clone();
    let bad = [
        // lines: required, 1..=5000 (1..=1000 until 2026-10-01), an integer.
        json!({"end":0}),
        json!({"end":0,"lines":0}),
        json!({"end":0,"lines":5001}),
        json!({"end":0,"lines":4294967295u64}),
        json!({"end":0,"lines":4294967296u64}),
        json!({"end":0,"lines":-1}),
        json!({"end":0,"lines":1.5}),
        json!({"end":0,"lines":"10"}),
        json!({"end":0,"lines":null}),
        json!({"end":0,"lines":[10]}),
        json!({"end":0,"lines":true}),
        // end: required, a u32.
        json!({"lines":10}),
        json!({"end":-1,"lines":10}),
        json!({"end":4294967296u64,"lines":10}),
        json!({"end":1.5,"lines":10}),
        json!({"end":"0","lines":10}),
        json!({"end":null,"lines":10}),
        json!({"end":false,"lines":10}),
        // styled: a boolean.
        json!({"end":0,"lines":10,"styled":"yes"}),
        json!({"end":0,"lines":10,"styled":1}),
        json!({"end":0,"lines":10,"styled":[]}),
        // Nothing else is a parameter, least of all a wait.
        json!({"end":0,"lines":10,"unknown":true}),
        json!({"end":0,"lines":10,"if_changed":"h"}),
        json!({"end":0,"lines":10,"wait_ms":100}),
        json!({"end":0,"lines":10,"hash":"h"}),
        json!({"end":0,"lines":10,"shell":"x"}),
    ];
    for params in bad {
        let mut with_shell = params.clone();
        with_shell["shell_id"] = json!(shell);
        let response = f.call(req("shell.history", with_shell)).await;
        assert_eq!(code(&response), "invalid_request", "{params}: {response}");
    }
    // The shell itself: missing, not a string, not a UUID, not canonical.
    for shell_id in [
        None,
        Some(json!(7)),
        Some(json!(null)),
        Some(json!("nope")),
        Some(json!("")),
        Some(json!(shell.to_uppercase())),
        Some(json!(shell.replace('-', ""))),
        Some(json!(&shell[..8])),
    ] {
        let mut params = json!({"end":0,"lines":10});
        if let Some(shell_id) = &shell_id {
            params["shell_id"] = shell_id.clone();
        }
        let response = f.call(req("shell.history", params)).await;
        assert_eq!(
            code(&response),
            "invalid_request",
            "{shell_id:?}: {response}"
        );
    }
    let response = f.call(req("shell.history", json!([]))).await;
    assert_eq!(code(&response), "invalid_request");
    let response = f.call(req("shell.history", json!(null))).await;
    assert_eq!(code(&response), "invalid_request");
    assert!(!f.stub.path().join("argv.log").exists());
}

#[tokio::test]
async fn the_bounds_are_inclusive_and_null_means_absent_for_styled() {
    let f = Fixture::new();
    f.cli_says(page(&f.shell, "x", 1, 5, true));
    for params in [
        json!({"end":0,"lines":1}),
        json!({"end":0,"lines":1000}),
        json!({"end":0,"lines":5000}),
        json!({"end":4294967295u64,"lines":1}),
        json!({"end":4294967295u64,"lines":1000,"styled":true}),
        json!({"end":7,"lines":10,"styled":false}),
        json!({"end":7,"lines":10,"styled":null}),
    ] {
        let response = f.history(params.clone()).await;
        assert_eq!(response["ok"], true, "{params}: {response}");
    }
}

#[tokio::test]
async fn the_request_becomes_the_cli_arguments_and_the_page_is_passed_on() {
    let f = Fixture::new();
    f.cli_says(page(&f.shell, "a\n\nc", 3, 500, false));
    let response = f.history(json!({"end":40,"lines":3})).await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["result"],
        json!({"shell_id":f.shell,"output":"a\n\nc","line_count":3,
               "history_size":500,"complete":false})
    );
    assert_eq!(
        f.history_call(),
        [
            "shell", "history", &f.shell, "--end", "40", "--lines", "3", "--json"
        ]
    );
    // Styled adds exactly one flag; the largest end is not truncated.
    f.cli_says(page(&f.shell, "\u{1b}[31ma\u{1b}[0m", 1, 9, true));
    f.history(json!({"end":4294967295u64,"lines":1000,"styled":true}))
        .await;
    assert_eq!(
        f.history_call(),
        [
            "shell",
            "history",
            &f.shell,
            "--end",
            "4294967295",
            "--lines",
            "1000",
            "--styled",
            "--json"
        ]
    );
    // Nothing the CLI printed beyond the page travels with it.
    f.cli_says(json!({
        "id": f.shell, "output": "a", "line_count": 1, "history_size": 9,
        "complete": true, "cursor": {"x":0,"y":0}, "hash": "0123456789abcdef"
    }));
    let response = f.history(json!({"end":0,"lines":5})).await;
    assert_eq!(
        response["result"],
        json!({"shell_id":f.shell,"output":"a","line_count":1,"history_size":9,"complete":true})
    );
}

#[tokio::test]
async fn an_empty_page_and_one_blank_line_are_different_answers() {
    let f = Fixture::new();
    f.cli_says(page(&f.shell, "", 0, 40, true));
    let response = f.history(json!({"end":40,"lines":10})).await;
    assert_eq!(
        response["result"],
        json!({"shell_id":f.shell,"output":"","line_count":0,"history_size":40,"complete":true})
    );
    f.cli_says(page(&f.shell, "", 1, 40, false));
    let response = f.history(json!({"end":3,"lines":1})).await;
    assert_eq!(
        response["result"],
        json!({"shell_id":f.shell,"output":"","line_count":1,"history_size":40,"complete":false})
    );
    // Blank lines at the ends of a page are kept as they came.
    f.cli_says(page(&f.shell, "\n\n", 3, 40, false));
    let response = f.history(json!({"end":3,"lines":3})).await;
    assert_eq!(response["result"]["output"], "\n\n");
    assert_eq!(response["result"]["line_count"], 3);
}

#[tokio::test]
async fn a_page_that_does_not_fit_the_request_is_a_cli_error() {
    let f = Fixture::new();
    let s = f.shell.clone();
    let cases = [
        // Not a page at all.
        json!({"id":s}),
        json!({"id":s,"output":"a","line_count":1,"history_size":5}),
        json!({"id":s,"output":"a","line_count":1,"complete":true}),
        json!({"id":s,"line_count":1,"history_size":5,"complete":true}),
        json!({"id":s,"output":"a","history_size":5,"complete":true}),
        // Wrong types and ranges.
        json!({"id":s,"output":7,"line_count":1,"history_size":5,"complete":true}),
        json!({"id":s,"output":"a","line_count":"1","history_size":5,"complete":true}),
        json!({"id":s,"output":"a","line_count":-1,"history_size":5,"complete":true}),
        json!({"id":s,"output":"a","line_count":1.5,"history_size":5,"complete":true}),
        json!({"id":s,"output":"a","line_count":1,"history_size":4294967296u64,"complete":true}),
        json!({"id":s,"output":"a","line_count":1,"history_size":5,"complete":"true"}),
        // The count is not the number of lines.
        json!({"id":s,"output":"a\nb","line_count":1,"history_size":5,"complete":true}),
        json!({"id":s,"output":"a","line_count":2,"history_size":5,"complete":true}),
        json!({"id":s,"output":"a","line_count":0,"history_size":5,"complete":true}),
        json!({"id":s,"output":"","line_count":2,"history_size":5,"complete":true}),
        // More than was asked for (the request below asks for 2), or than exists.
        json!({"id":s,"output":"a\nb\nc","line_count":3,"history_size":5,"complete":false}),
        json!({"id":s,"output":"a\nb","line_count":2,"history_size":1,"complete":true}),
        // Nothing, yet not at the top.
        json!({"id":s,"output":"","line_count":0,"history_size":5,"complete":false}),
    ];
    for cli in cases {
        f.cli_says(cli.clone());
        let response = f.history(json!({"end":0,"lines":2})).await;
        assert_eq!(code(&response), "cli_error", "{cli}: {response}");
    }
    f.set("history.json", "not json");
    assert_eq!(
        code(&f.history(json!({"end":0,"lines":2})).await),
        "cli_error"
    );
    f.set("history.json", "");
    assert_eq!(
        code(&f.history(json!({"end":0,"lines":2})).await),
        "cli_error"
    );
}

#[tokio::test]
async fn styled_pages_carry_sgr_and_nothing_else_or_the_request_fails() {
    let f = Fixture::new();
    let sgr = "\u{1b}[1;31mred\u{1b}[0m \u{1b}[38;2;1;2;3mtrue\u{1b}[0m\n\u{1b}[0mnext";
    f.cli_says(page(&f.shell, sgr, 2, 10, true));
    let response = f.history(json!({"end":0,"lines":5,"styled":true})).await;
    assert_eq!(response["result"]["output"], sgr, "{response}");
    for dirty in [
        "a\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\",
        "a\u{1b}[2Jb",
        "a\u{1b}[?25lb",
        "a\u{1b}(0b",
        "a\u{e}b",
        "a\rb",
        "a\u{1b}[31",
        "a\u{1b}",
    ] {
        f.cli_says(page(&f.shell, dirty, 1, 10, true));
        let response = f.history(json!({"end":0,"lines":5,"styled":true})).await;
        assert_eq!(code(&response), "cli_error", "{dirty:?}: {response}");
        // Not asked for styled, the page is not judged as such.
        let response = f.history(json!({"end":0,"lines":5})).await;
        assert_eq!(response["ok"], true, "{dirty:?}: {response}");
        assert_eq!(response["result"]["output"], dirty);
    }
}

#[tokio::test]
async fn an_unknown_or_dead_shell_is_not_found_and_the_cli_is_not_asked_for_history() {
    let f = Fixture::new();
    f.cli_says(page(&f.shell, "a", 1, 5, true));
    let response = f
        .call(req(
            "shell.history",
            json!({"shell_id":new_uuid(),"end":0,"lines":5}),
        ))
        .await;
    assert_eq!(code(&response), "not_found", "{response}");
    f.set("dead", "");
    let response = f.history(json!({"end":0,"lines":5})).await;
    assert_eq!(code(&response), "not_found", "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not alive")
    );
    assert_eq!(f.history_calls(), 0);
    // A validation error still wins over a missing shell.
    let response = f
        .call(req(
            "shell.history",
            json!({"shell_id":new_uuid(),"end":0,"lines":0}),
        ))
        .await;
    assert_eq!(code(&response), "invalid_request");
}

#[tokio::test]
async fn cli_failures_are_cli_errors_and_an_old_cli_is_named() {
    let f = Fixture::new();
    f.set("mode", "old");
    let response = f.history(json!({"end":0,"lines":5})).await;
    assert_eq!(code(&response), "cli_error");
    let message = response["error"]["message"].as_str().unwrap();
    assert!(message.contains("update RiWork"), "{message}");
    f.set("mode", "fail");
    let response = f.history(json!({"end":0,"lines":5})).await;
    assert_eq!(code(&response), "cli_error");
    let message = response["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("no server running") && !message.contains("update RiWork"),
        "{message}"
    );
    // A CLI that cannot even be started.
    let gone = Fixture::new();
    let rpc = Rpc::new("/nonexistent/riwork".into(), gone.rpc.storage.clone());
    let response = rpc
        .handle(
            &gone.device,
            req(
                "shell.history",
                json!({"shell_id":gone.shell,"end":0,"lines":5}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(code(&response), "cli_error");
}

#[tokio::test]
async fn a_page_the_response_cannot_carry_is_response_too_large() {
    let f = Fixture::new();
    // The CLI prints more than one encrypted response can hold (128 KiB).
    let long = "x".repeat(140_000);
    f.cli_says(page(&f.shell, &long, 1, 9, true));
    let response = f.history(json!({"end":0,"lines":1000})).await;
    assert_eq!(code(&response), "response_too_large", "{response}");
    let message = response["error"]["message"].as_str().unwrap();
    assert!(message.contains("fewer lines"), "{message}");
    // Just inside the limit is fine; the client halves `lines` and retries.
    let fits = "x".repeat(100_000);
    f.cli_says(page(&f.shell, &fits, 1, 9, true));
    let response = f.history(json!({"end":0,"lines":500})).await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["result"]["output"].as_str().unwrap().len(),
        100_000
    );
}

#[tokio::test]
async fn the_request_id_is_echoed_in_the_response() {
    let f = Fixture::new();
    f.cli_says(page(&f.shell, "a", 1, 5, true));
    let request = req(
        "shell.history",
        json!({"shell_id":f.shell,"end":0,"lines":5}),
    );
    let response = f.call(request.clone()).await;
    assert_eq!(response["id"], request["id"]);
    assert_eq!(response["type"], "response");
    assert_eq!(response["ok"], true);
    assert_eq!(f.history_calls(), 1);
}
