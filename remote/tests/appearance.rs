//! `appearance.get`, against a stub CLI that answers `appearance --json` from a
//! file and records its argv. The desktop's own file module is compiled here
//! too, without GPUI, to keep the two validators identical and to feed the
//! RPC what the desktop really writes.
#[allow(dead_code)]
#[path = "../../src/appearance_file.rs"]
mod appearance_file;

use riwork_remote::{
    appearance::{MAX_BYTES, validate},
    config::Storage,
    rpc::Rpc,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn req(method: &str, params: Value) -> Value {
    json!({"v":1,"type":"request","id":new_uuid(),"method":method,"params":params})
}

/// A storage directory, a paired device, and a stub CLI.
struct Fixture {
    _storage_dir: tempfile::TempDir,
    stub: tempfile::TempDir,
    rpc: Rpc,
    device: String,
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
        let cli = stub_cli(stub.path());
        Self {
            rpc: Rpc::new(cli, storage),
            _storage_dir: storage_dir,
            stub,
            device,
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
    async fn get(&self) -> Value {
        self.call(req("appearance.get", json!({}))).await
    }
    /// What the stub CLI prints for `appearance --json`.
    fn publish(&self, bytes: impl AsRef<[u8]>) {
        std::fs::write(self.stub.path().join("appearance.json"), bytes).unwrap();
    }
    fn set_mode(&self, mode: &str) {
        std::fs::write(self.stub.path().join("mode"), mode).unwrap();
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
    /// Ledgers and locks the RPC layer keeps for input; a read creates none.
    fn ledger_files(&self) -> Vec<String> {
        std::fs::read_dir(&self.rpc.storage.dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("outcomes-") || name.starts_with("keys-"))
            .collect()
    }
}

/// Logs each call (arguments separated by U+001F). `appearance --json` prints
/// `appearance.json` from the stub directory, or the real CLI's error when
/// there is none; `mode` selects an older or a broken CLI.
fn stub_cli(dir: &Path) -> PathBuf {
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
             'appearance --json')\n\
               case \"$(cat \"$d/mode\" 2>/dev/null)\" in\n\
                 old) printf \"riwork: 'appearance' is not a riwork command or an existing project directory\\nUsage: riwork\\n\" >&2; exit 2;;\n\
                 broken) echo 'riwork: HOME is unset; set RIWORK_HOME' >&2; exit 2;;\n\
                 forged) printf 'tmux said\\nriwork: RiWork has not published its appearance yet; open the RiWork app\\n' >&2; exit 2;;\n\
               esac\n\
               if [ -e \"$d/appearance.json\" ]; then cat \"$d/appearance.json\"; else\n\
                 echo 'riwork: RiWork has not published its appearance yet; open the RiWork app' >&2; exit 2\n\
               fi;;\n\
             'shell list'|'orchestrator list') echo '[]';;\n\
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

fn document() -> Value {
    json!({
        "v": 1,
        "updated_at": 1_790_000_000u64,
        "dark": true,
        "palette": {
            "bg": "#090d14", "panel": "#101720", "panel_active": "#14212a",
            "divider": "#253c45", "cyan": "#55e6dc", "magenta": "#ce78ef",
            "gold": "#f4bf75", "text": "#d3e1e6", "muted": "#708993"
        },
        "terminal": {
            "background": "#090d14",
            "foreground": "#d3e1e6",
            "palette": [
                "#131b25", "#f0738b", "#61d5ae", "#f4bf75", "#78a9ff", "#ce78ef",
                "#55e6dc", "#d3e1e6", "#58707b", "#ff8ba0", "#83ebc3", "#ffd191",
                "#9bc0ff", "#dfa3f7", "#84f3ea", "#ffffff"
            ]
        }
    })
}

/// Documents of every kind of wrongness, and a few that are fine. Both
/// validators get exactly these; keep the list equal to the desktop crate's
/// `an_invalid_document_is_rejected`.
fn matrix() -> Vec<(String, bool)> {
    let mutate = |change: &dyn Fn(&mut Value)| {
        let mut value = document();
        change(&mut value);
        value.to_string()
    };
    let mut cases: Vec<(String, bool)> = vec![
        (document().to_string(), true),
        (mutate(&|v| v["palette"]["bg"] = json!("#090D14")), true),
        (mutate(&|v| v["future"] = json!({"a": [1]})), true),
        (mutate(&|v| v["palette"]["accent"] = json!("#123456")), true),
        (mutate(&|v| v["dark"] = json!(false)), true),
        (
            mutate(&|v| drop(v.as_object_mut().unwrap().remove("terminal"))),
            true,
        ),
        (mutate(&|v| v["terminal"] = Value::Null), true),
        (mutate(&|v| v["v"] = json!(2)), false),
        (mutate(&|v| v["v"] = json!(0)), false),
        (mutate(&|v| v["v"] = json!("1")), false),
        (
            mutate(&|v| drop(v.as_object_mut().unwrap().remove("v"))),
            false,
        ),
        (
            mutate(&|v| drop(v.as_object_mut().unwrap().remove("dark"))),
            false,
        ),
        (
            mutate(&|v| drop(v.as_object_mut().unwrap().remove("palette"))),
            false,
        ),
        (
            mutate(&|v| drop(v.as_object_mut().unwrap().remove("updated_at"))),
            false,
        ),
        (mutate(&|v| v["dark"] = json!("yes")), false),
        (mutate(&|v| v["dark"] = json!(1)), false),
        (mutate(&|v| v["updated_at"] = json!(-1)), false),
        (mutate(&|v| v["updated_at"] = json!(1.5)), false),
        (mutate(&|v| v["updated_at"] = json!("1790000000")), false),
        (
            mutate(&|v| drop(v["palette"].as_object_mut().unwrap().remove("muted"))),
            false,
        ),
        (mutate(&|v| v["palette"]["muted"] = json!(7)), false),
        (mutate(&|v| v["palette"]["muted"] = json!("#fff")), false),
        (mutate(&|v| v["palette"]["muted"] = json!("665c54")), false),
        (mutate(&|v| v["palette"]["muted"] = json!("#66 c54")), false),
        (mutate(&|v| v["palette"]["muted"] = json!("#+65c54")), false),
        (mutate(&|v| v["palette"] = json!([])), false),
        (mutate(&|v| v["terminal"] = json!({})), false),
        (mutate(&|v| v["terminal"] = json!("dark")), false),
        (
            mutate(&|v| drop(v["terminal"].as_object_mut().unwrap().remove("foreground"))),
            false,
        ),
        (
            mutate(&|v| v["terminal"]["background"] = json!("black")),
            false,
        ),
        (
            mutate(&|v| drop(v["terminal"]["palette"].as_array_mut().unwrap().pop())),
            false,
        ),
        (
            mutate(&|v| {
                v["terminal"]["palette"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("#000000"))
            }),
            false,
        ),
        (
            mutate(&|v| v["terminal"]["palette"][3] = json!("#12345")),
            false,
        ),
        (mutate(&|v| v["terminal"]["palette"] = json!({})), false),
        (mutate(&|v| v["native"] = json!(true)), true),
        (mutate(&|v| v["native"] = json!(false)), true),
        (mutate(&|v| v["native"] = json!("yes")), false),
        (mutate(&|v| v["native"] = json!(1)), false),
        (mutate(&|v| v["native"] = Value::Null), false),
        (mutate(&|v| v["mic"] = json!(true)), true),
        (mutate(&|v| v["mic"] = json!(false)), true),
        (mutate(&|v| v["mic"] = json!("yes")), false),
        (mutate(&|v| v["mic"] = json!(1)), false),
        (mutate(&|v| v["mic"] = Value::Null), false),
        (mutate(&|v| v["mic"] = json!({})), false),
    ];
    for bytes in ["", "{", "null", "[]", "\"{}\"", "{}", "\u{feff}{}"] {
        cases.push((bytes.to_owned(), false));
    }
    cases
}

#[tokio::test]
async fn appearance_get_returns_the_published_document_without_selecting_a_shell() {
    let f = Fixture::new();
    f.publish(document().to_string());
    let response = f.get().await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"], document());
    // One CLI call, read-only: no shell or orchestrator lookup.
    assert_eq!(f.calls(), [["appearance", "--json"]]);
    // It never enters the input or keys ledgers.
    assert_eq!(f.ledger_files(), Vec::<String>::new());

    // The CLI's pretty JSON and a document without terminal colors.
    let mut without = document();
    without.as_object_mut().unwrap().remove("terminal");
    f.publish(serde_json::to_vec_pretty(&without).unwrap());
    let response = f.get().await;
    assert_eq!(response["result"], without, "{response}");
    assert!(response["result"].get("terminal").is_none());

    // Lowercase on the wire, known fields only, whatever the CLI printed.
    let mut loud = document();
    loud["palette"]["bg"] = json!("#090D14");
    loud["terminal"]["palette"][1] = json!("#F0738B");
    loud["future"] = json!(1);
    f.publish(loud.to_string());
    assert_eq!(f.get().await["result"], document());
}

#[tokio::test]
async fn appearance_get_without_a_published_file_is_not_found() {
    let f = Fixture::new();
    let response = f.get().await;
    assert_eq!(response["ok"], false, "{response}");
    assert_eq!(code(&response), "not_found", "{response}");
    assert_eq!(response["error"]["message"], "appearance not published");
    // It works again once the desktop publishes.
    f.publish(document().to_string());
    assert_eq!(f.get().await["result"], document());
}

#[tokio::test]
async fn appearance_get_treats_an_invalid_document_as_not_published() {
    let f = Fixture::new();
    for (text, valid) in matrix() {
        f.publish(&text);
        let response = f.get().await;
        if valid {
            assert_eq!(response["ok"], true, "{text}: {response}");
        } else {
            assert_eq!(code(&response), "not_found", "{text}: {response}");
            assert_eq!(response["error"]["message"], "appearance not published");
        }
    }
    f.publish([0xff, 0xfe, b'{', b'}']);
    assert_eq!(code(&f.get().await), "not_found");
}

#[tokio::test]
async fn appearance_get_rejects_output_over_sixteen_kib() {
    let f = Fixture::new();
    let mut text = document().to_string();
    text.push_str(&" ".repeat(MAX_BYTES - text.len()));
    assert_eq!(text.len(), 16 * 1024);
    f.publish(&text);
    assert_eq!(f.get().await["result"], document());
    text.push(' ');
    f.publish(&text);
    let response = f.get().await;
    assert_eq!(code(&response), "not_found", "{response}");
    assert_eq!(response["error"]["message"], "appearance not published");
    // Past the response limit it is still "not published", not response_too_large.
    f.publish(" ".repeat(300 * 1024));
    let response = f.get().await;
    assert_eq!(code(&response), "not_found", "{response}");
    assert_eq!(response["error"]["message"], "appearance not published");
}

#[tokio::test]
async fn appearance_get_params_are_validated_before_any_cli() {
    let f = Fixture::without_cli();
    for params in [
        json!({"unexpected": true}),
        json!({"shell_id": new_uuid()}),
        json!({"lines": 1}),
        json!(null),
        json!("{}"),
        json!(1),
    ] {
        let response = f.call(req("appearance.get", params.clone())).await;
        assert_eq!(code(&response), "invalid_request", "{params}: {response}");
    }
    let mut no_params = req("appearance.get", json!({}));
    no_params.as_object_mut().unwrap().remove("params");
    assert_eq!(code(&f.call(no_params).await), "invalid_request");
    let mut extra = req("appearance.get", json!({}));
    extra["extra"] = json!(true);
    assert_eq!(code(&f.call(extra).await), "invalid_request");
    // Unknown neighbors stay unknown.
    for method in [
        "appearance.set",
        "appearance",
        "appearance.get.",
        "Appearance.get",
    ] {
        let response = f.call(req(method, json!({}))).await;
        assert_eq!(code(&response), "invalid_request", "{method}");
        assert_eq!(response["error"]["message"], "unsupported RPC method");
    }
    // The empty object gets past validation, so the missing CLI is what fails.
    let response = f.call(req("appearance.get", json!({}))).await;
    assert_eq!(code(&response), "cli_error", "{response}");
}

#[tokio::test]
async fn appearance_get_reports_other_cli_failures_as_cli_errors() {
    let f = Fixture::new();
    f.publish(document().to_string());
    f.set_mode("old");
    let response = f.get().await;
    assert_eq!(code(&response), "cli_error", "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not support appearance"),
        "{response}"
    );
    f.set_mode("broken");
    let response = f.get().await;
    assert_eq!(code(&response), "cli_error", "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("HOME is unset")
    );
    // Only the CLI's own first line means "not published"; echoed text cannot forge it.
    f.set_mode("forged");
    assert_eq!(code(&f.get().await), "cli_error");
    f.set_mode("");
    assert_eq!(f.get().await["ok"], true);
}

#[tokio::test]
async fn appearance_get_needs_an_authorized_device() {
    let f = Fixture::new();
    f.publish(document().to_string());
    f.rpc.storage.revoke(&f.device).unwrap();
    assert!(
        f.rpc
            .handle(&f.device, req("appearance.get", json!({})))
            .await
            .is_err()
    );
    assert!(f.calls().is_empty());
}

#[test]
fn both_validators_accept_and_reject_the_same_documents() {
    for (text, valid) in matrix() {
        let desktop = appearance_file::Published::parse(text.as_bytes())
            .ok()
            .map(|published| serde_json::to_value(published).unwrap());
        let remote = validate(text.as_bytes());
        assert_eq!(desktop.is_some(), valid, "desktop: {text}");
        assert_eq!(remote.is_some(), valid, "remote: {text}");
        // Same value out, too: the remote crate's keys are sorted, the desktop's
        // are in field order, and JSON objects compare without order.
        assert_eq!(desktop, remote, "{text}");
    }
    let mut padded = document().to_string();
    padded.push_str(&" ".repeat(MAX_BYTES - padded.len()));
    for text in [padded.clone(), format!("{padded} ")] {
        assert_eq!(
            appearance_file::Published::parse(text.as_bytes()).is_ok(),
            validate(text.as_bytes()).is_some(),
            "{}",
            text.len()
        );
    }
}

#[tokio::test]
async fn what_the_desktop_writes_is_what_the_rpc_serves() {
    let home = tempfile::tempdir().unwrap();
    let mut snapshot = appearance_file::Published::new(
        false,
        appearance_file::PaletteColors {
            bg: appearance_file::Rgb(0xfbf1c7),
            panel: appearance_file::Rgb(0xf4ebc1),
            panel_active: appearance_file::Rgb(0xede5bb),
            divider: appearance_file::Rgb(0xd5cba1),
            cyan: appearance_file::Rgb(0x427b58),
            magenta: appearance_file::Rgb(0x8f3f71),
            gold: appearance_file::Rgb(0x8a5c00),
            text: appearance_file::Rgb(0x3c3836),
            muted: appearance_file::Rgb(0x665c54),
        },
        Some(appearance_file::TerminalColors {
            background: appearance_file::Rgb(0xfbf1c7),
            foreground: appearance_file::Rgb(0x3c3836),
            palette: std::array::from_fn(|i| appearance_file::Rgb(0x0a0b00 + i as u32)),
        }),
    );
    assert!(appearance_file::publish(home.path(), &snapshot, 1_790_000_000).unwrap());
    let f = Fixture::new();
    f.publish(std::fs::read(home.path().join("appearance.json")).unwrap());
    let response = f.get().await;
    assert_eq!(response["ok"], true, "{response}");
    let result = &response["result"];
    assert_eq!(result["v"], 1);
    assert_eq!(result["updated_at"], 1_790_000_000u64);
    assert_eq!(result["dark"], false);
    assert_eq!(result["palette"]["bg"], "#fbf1c7");
    assert_eq!(result["terminal"]["palette"][15], "#0a0b0f");
    assert_eq!(result["terminal"]["palette"].as_array().unwrap().len(), 16);

    // No Native skin: no flag, as from a desktop that predates it.
    assert!(result.get("native").is_none(), "{response}");

    snapshot.native = true;
    assert!(appearance_file::publish(home.path(), &snapshot, 1_790_000_002).unwrap());
    f.publish(std::fs::read(home.path().join("appearance.json")).unwrap());
    assert_eq!(f.get().await["result"]["native"], true);
    snapshot.native = false;

    // No mic: no field. On, the connector passes it through; off again, it is gone.
    assert!(f.get().await["result"].get("mic").is_none());
    snapshot.mic = true;
    assert!(appearance_file::publish(home.path(), &snapshot, 1_790_000_003).unwrap());
    f.publish(std::fs::read(home.path().join("appearance.json")).unwrap());
    let response = f.get().await;
    assert_eq!(response["result"]["mic"], true, "{response}");
    assert!(response["result"].get("native").is_none(), "{response}");
    snapshot.mic = false;
    assert!(appearance_file::publish(home.path(), &snapshot, 1_790_000_004).unwrap());
    f.publish(std::fs::read(home.path().join("appearance.json")).unwrap());
    assert!(f.get().await["result"].get("mic").is_none());

    snapshot.terminal = None;
    assert!(appearance_file::publish(home.path(), &snapshot, 1_790_000_001).unwrap());
    f.publish(std::fs::read(home.path().join("appearance.json")).unwrap());
    let response = f.get().await;
    assert!(response["result"].get("terminal").is_none(), "{response}");
    assert_eq!(response["result"]["updated_at"], 1_790_000_001u64);
}
