//! Exercise the real dispatcher in child processes, each with a private
//! RIWORK_HOME, HOME and runtime directory. No installed CLI or desktop is used.

use riwork_core::settings::{Settings, SettingsStore};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
};
use uuid::Uuid;

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("riwork-settings-cli-{}", Uuid::new_v4()));
        riwork_core::paths::create_private_dir(&root).unwrap();
        fs::create_dir_all(root.join("user")).unwrap();
        SettingsStore::open(root.join("state")).unwrap();
        Self(root)
    }

    fn file(&self) -> PathBuf {
        self.0.join("state/settings.json")
    }

    fn write(&self, value: Value) {
        fs::write(self.file(), serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    }

    fn document(&self) -> Value {
        serde_json::from_slice(&fs::read(self.file()).unwrap()).unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_riwork-cli"))
            .args(args)
            .current_dir(&self.0)
            .env("RIWORK_HOME", self.0.join("state"))
            .env("HOME", self.0.join("user"))
            .env("RIWORK_RUNTIME_DIR", self.0.join("runtime"))
            .env_remove("RIWORK_SHELL_ID")
            .env_remove("RIWORK_CHAT_ID")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty(), "{args:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.ok(args)).unwrap()
    }

    fn refused(&self, args: &[&str], code: &str) -> Value {
        let output = self.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], code, "{args:?}: {value}");
        assert!(value["error"]["message"].is_string());
        assert_eq!(value.as_object().unwrap().len(), 1);
        value
    }

    fn no_temporary_or_project_files(&self) {
        for file in fs::read_dir(self.0.join("state")).unwrap() {
            let name = file.unwrap().file_name();
            assert!(
                name == "settings.json" || name == "settings.lock",
                "{name:?}"
            );
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // This UUID directory belongs only to this fixture.
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn list_and_get_show_every_known_typed_key_and_sorted_compact_text() {
    let sandbox = Sandbox::new();
    let defaults = Settings::default().values().unwrap();
    let listed = sandbox.json(&["settings", "list", "--json"]);
    assert_eq!(listed, json!({"v":1,"settings":defaults}));
    assert!(defaults.contains_key("claude_chat_ignores_api_key"));
    assert!(!defaults.contains_key("claude_chat_billing"));
    assert!(!defaults.contains_key("orchestrator_unreadable"));
    let mut lines = Vec::new();
    for (key, value) in &defaults {
        assert_eq!(
            sandbox.json(&["settings", "get", key, "--json"]),
            json!({"v":1,"key":key,"value":value})
        );
        assert_eq!(sandbox.ok(&["settings", "get", key]), format!("{value}\n"));
        lines.push(format!("{key} = {value}"));
    }
    lines.sort();
    assert_eq!(sandbox.ok(&["settings", "list"]), lines.join("\n") + "\n");
    assert!(
        !sandbox.file().exists(),
        "reading defaults must not save them"
    );
    sandbox.no_temporary_or_project_files();
}

#[test]
fn every_persisted_key_can_be_set_back_to_its_typed_value() {
    let sandbox = Sandbox::new();
    let defaults = Settings::default().values().unwrap();
    for (key, value) in &defaults {
        let raw = value.to_string();
        assert_eq!(
            sandbox.json(&["settings", "set", key, "--json", &raw]),
            json!({"v":1,"key":key,"value":value}),
            "{key}"
        );
    }
    assert_eq!(
        sandbox.json(&["settings", "list", "--json"])["settings"],
        json!(defaults)
    );
    sandbox.no_temporary_or_project_files();
}

#[test]
fn scalar_enum_nullable_and_billing_values_round_trip() {
    let sandbox = Sandbox::new();
    for (key, input, expected) in [
        ("dictation_mic", "true", json!(true)),
        ("codex_subscription_balancing", "true", json!(true)),
        ("dashboard_haiku_summaries", "true", json!(true)),
        ("theme", "native", json!("native")),
        ("tab_close_behavior", "detach", json!("detach")),
        ("orchestrator_mode", "chat", json!("chat")),
        ("orchestrator_chat_provider", "claude", json!("claude")),
        ("orchestrator_chat_model", "opus", json!("opus")),
        ("orchestrator_chat_model", "null", Value::Null),
        (
            "selected_codex_account",
            "private-test-account",
            json!("private-test-account"),
        ),
        ("selected_codex_account", "null", Value::Null),
        ("claude_chat_ignores_api_key", "true", json!(true)),
    ] {
        assert_eq!(
            sandbox.json(&["settings", "set", key, "--json", input]),
            json!({"v":1,"key":key,"value":expected})
        );
        assert_eq!(
            sandbox.json(&["settings", "get", key, "--json"])["value"],
            expected
        );
    }
    assert_eq!(
        sandbox.ok(&["settings", "set", "theme", "\"native\""]),
        "\"native\"\n"
    );
    assert_eq!(sandbox.document()["theme"], "native");
    assert_eq!(sandbox.document()["claude_chat_ignores_api_key"], true);
}

#[test]
fn set_prints_model_normalization_and_supports_nested_object_values() {
    let sandbox = Sandbox::new();
    for (raw, expected) in [("3", json!(9)), ("100", json!(24)), ("12.34", json!(12.3))] {
        assert_eq!(
            sandbox.json(&["settings", "set", "ui_text_size", raw, "--json"])["value"],
            expected
        );
        assert_eq!(sandbox.document()["ui_text_size"], expected);
    }
    let status = sandbox.json(&[
        "settings",
        "set",
        "status_bar",
        "--json",
        r#"{"enabled":false,"usage_mode":"all_accounts","items":[{"kind":"usage","side":"left"}]}"#,
    ])["value"]
        .clone();
    assert_eq!(status["enabled"], false);
    assert_eq!(status["usage_mode"], "all_accounts");
    assert_eq!(
        status["items"][0],
        json!({"kind":"usage","side":"left","enabled":true})
    );
    assert_eq!(status["items"].as_array().unwrap().len(), 11);
    assert_eq!(sandbox.document()["status_bar"], status);
    // Top-level object replacement defaults omitted fields rather than patching them.
    let reset = sandbox.json(&[
        "settings",
        "set",
        "status_bar",
        r#"{"usage_mode":"all_accounts"}"#,
        "--json",
    ]);
    assert_eq!(reset["value"]["enabled"], true);
    for (key, raw) in [
        ("project_order", r#"{"by":"name","descending":false}"#),
        ("chat_display_modes", r#"{"private-chat":"verbose"}"#),
        (
            "delegation_policy",
            r#"{"spread_work":false,"parallel_shells":3,"design":{"provider":"claude","model":"opus"}}"#,
        ),
    ] {
        let expected: Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            sandbox.json(&["settings", "set", key, raw, "--json"])["value"],
            expected
        );
        assert_eq!(
            sandbox.json(&["settings", "get", key, "--json"])["value"],
            expected
        );
    }
}

#[test]
fn wrong_types_unknown_components_and_lenient_fallbacks_never_write() {
    let sandbox = Sandbox::new();
    sandbox.write(json!({"schema_version":1,"theme":"tokyo_night","future":{"keep":[1,2]}}));
    let before = fs::read(sandbox.file()).unwrap();
    for (key, raw) in [
        ("dictation_mic", "\"true\""),
        ("remember_window_size", "null"),
        ("theme", "3"),
        ("theme", "future_theme"),
        ("theme", "tokyo_night"),
        ("chat_display", "loud"),
        ("tab_close_behavior", "close"),
        ("agent_inline_mode", "[]"),
        ("agent_terminals_unrestricted", "{}"),
        ("selected_codex_account", "false"),
        ("codex_subscription_balancing", "\"true\""),
        ("dashboard_haiku_summaries", "null"),
        ("schema_version", "2"),
        ("schema_version", "null"),
        ("schema_version", "\"1\""),
        ("orchestrator_mode", "hologram"),
        ("orchestrator_chat_provider", "grok"),
        ("orchestrator_chat_model", "42"),
        ("orchestrator_chat_model", "\"  \""),
        ("orchestrator_chat_model", "\"a\\nb\""),
        ("ui_text_size", "\"12\""),
        ("ui_text_size", "null"),
        ("interface_font", "comic_sans"),
        ("claude_chat_ignores_api_key", "\"true\""),
        ("chat_display_modes", r#"{"a":"verbose","b":"future"}"#),
        ("chat_display_modes", "[]"),
        ("project_order", r#"{"by":"name","descending":"false"}"#),
        ("project_order", r#"{"by":"name","typo":false}"#),
        ("status_bar", "null"),
        ("status_bar", r#"{"enabled":"true"}"#),
        ("status_bar", r#"{"usage_mode":"per_provider"}"#),
        ("status_bar", r#"{"items":"usage"}"#),
        ("status_bar", r#"{"items":[{"enabled":false}]}"#),
        ("status_bar", r#"{"items":[{"kind":"future"}]}"#),
        (
            "status_bar",
            r#"{"items":[{"kind":"usage","enabled":"true"}]}"#,
        ),
        ("status_bar", r#"{"items":[{"kind":"usage","typo":true}]}"#),
        (
            "status_bar",
            r#"{"items":[{"kind":"usage"},{"kind":"usage"}]}"#,
        ),
        ("status_bar", r#"{"typo":false}"#),
        ("delegation_policy", "false"),
        ("delegation_policy", r#"{"parallel_shells":11}"#),
        ("delegation_policy", r#"{"spread_work":null}"#),
        ("delegation_policy", r#"{"future":true}"#),
        (
            "delegation_policy",
            r#"{"design":{"provider":"claude","model":" "}}"#,
        ),
        (
            "delegation_policy",
            r#"{"design":{"provider":"claude","model":"opus","effort":3}}"#,
        ),
    ] {
        let error = sandbox.refused(&["settings", "set", key, raw, "--json"], "invalid_request");
        assert!(
            error["error"]["message"].as_str().unwrap().contains(key),
            "{error}"
        );
        assert_eq!(fs::read(sandbox.file()).unwrap(), before, "{key} {raw}");
        sandbox.no_temporary_or_project_files();
    }
    let output = sandbox.run(&["settings", "set", "theme", "false"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Cannot set 'theme'"));
    assert_eq!(fs::read(sandbox.file()).unwrap(), before);
}

#[test]
fn unknown_future_keys_and_runtime_diagnostics_are_not_cli_settings() {
    let sandbox = Sandbox::new();
    sandbox.write(json!({"future_setting":{"secret":"kept"}}));
    let before = fs::read(sandbox.file()).unwrap();
    for key in [
        "future_setting",
        "status_bar.usage_mode",
        "orchestrator_unreadable",
        "claude_chat_billing",
    ] {
        for args in [
            vec!["settings", "get", key, "--json"],
            vec!["settings", "set", key, "true", "--json"],
        ] {
            let error = sandbox.refused(&args, "invalid_request");
            assert!(
                error["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("not a known setting")
            );
        }
    }
    assert!(
        sandbox.json(&["settings", "list", "--json"])["settings"]
            .get("future_setting")
            .is_none()
    );
    assert_eq!(fs::read(sandbox.file()).unwrap(), before);
}

#[test]
fn unrelated_sets_preserve_future_fields_and_chat_map_updates_preserve_unknown_modes() {
    let sandbox = Sandbox::new();
    let saved = json!({
        "schema_version":1,"theme":"future_theme", "future":{"nested":[false,{"x":1}]},
        "status_bar":{"usage_mode":"per_provider","future_widget":true},
        "chat_display_modes":{"known":"normal","future-chat":"cinema"},
        "delegation_policy":{"future_policy":{"keep":true},"parallel_shells":30}
    });
    sandbox.write(saved.clone());
    sandbox.ok(&["settings", "set", "dictation_mic", "true"]);
    for key in [
        "theme",
        "future",
        "status_bar",
        "chat_display_modes",
        "delegation_policy",
    ] {
        assert_eq!(sandbox.document()[key], saved[key], "{key}");
    }
    assert_eq!(sandbox.ok(&["settings", "get", "theme"]), "\"native\"\n");
    sandbox.ok(&[
        "settings",
        "set",
        "chat_display_modes",
        r#"{"known":"verbose"}"#,
    ]);
    assert_eq!(
        sandbox.document()["chat_display_modes"],
        json!({"known":"verbose","future-chat":"cinema"})
    );
    sandbox.no_temporary_or_project_files();
}

#[test]
fn explicitly_choosing_a_default_replaces_an_unreadable_saved_value() {
    let sandbox = Sandbox::new();
    sandbox.write(json!({
        "theme":"catppuccin", "remember_window_size":"future", "orchestrator_mode":"hologram",
        "orchestrator_chat_provider":"future", "orchestrator_chat_model":42,
        "delegation_policy":"future", "future_field":true
    }));
    for (key, raw, expected) in [
        ("theme", "native", json!("native")),
        ("remember_window_size", "true", json!(true)),
        ("orchestrator_mode", "automatic", json!("automatic")),
        ("orchestrator_chat_provider", "codex", json!("codex")),
        ("orchestrator_chat_model", "null", Value::Null),
        ("delegation_policy", "{}", json!({})),
    ] {
        assert_eq!(
            sandbox.json(&["settings", "set", key, raw, "--json"])["value"],
            expected
        );
        assert_eq!(sandbox.document()[key], expected);
    }
    assert_eq!(sandbox.document()["future_field"], true);
}

#[test]
fn invalid_existing_files_follow_store_errors_and_are_never_replaced() {
    let sandbox = Sandbox::new();
    for raw in [
        "{broken",
        "[]",
        "null",
        "",
        r#"{"schema_version":2}"#,
        r#"{"schema_version":"1"}"#,
        r#"{"selected_codex_account":42}"#,
    ] {
        fs::write(sandbox.file(), raw).unwrap();
        for args in [
            vec!["settings", "list", "--json"],
            vec!["settings", "get", "theme", "--json"],
            vec!["settings", "set", "dictation_mic", "true", "--json"],
        ] {
            let error = sandbox.refused(&args, "cli_error");
            let store_error = SettingsStore::open(sandbox.0.join("state"))
                .unwrap()
                .load()
                .unwrap_err();
            assert_eq!(error["error"]["message"], store_error);
            assert_eq!(fs::read_to_string(sandbox.file()).unwrap(), raw);
        }
        sandbox.no_temporary_or_project_files();
    }
}

#[test]
fn refusals_with_no_existing_file_do_not_create_a_settings_document() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["settings", "set", "theme", "false", "--json"],
        vec!["settings", "set", "future", "true", "--json"],
        vec!["settings", "get", "future", "--json"],
    ] {
        sandbox.refused(&args, "invalid_request");
        assert!(!sandbox.file().exists());
    }
    sandbox.no_temporary_or_project_files();
}

#[test]
fn model_validation_uses_the_whole_saved_document() {
    let sandbox = Sandbox::new();
    sandbox.write(json!({"orchestrator_chat_provider":"future_provider"}));
    let before = fs::read(sandbox.file()).unwrap();
    sandbox.refused(
        &[
            "settings",
            "set",
            "orchestrator_chat_model",
            "opus",
            "--json",
        ],
        "invalid_request",
    );
    assert_eq!(fs::read(sandbox.file()).unwrap(), before);
    sandbox.ok(&["settings", "set", "orchestrator_chat_provider", "claude"]);
    assert_eq!(
        sandbox.ok(&["settings", "set", "orchestrator_chat_model", "opus"]),
        "\"opus\"\n"
    );
}

#[test]
fn concurrent_cli_and_app_store_updates_preserve_each_other() {
    use std::sync::{Arc, Barrier};
    let sandbox = Arc::new(Sandbox::new());
    sandbox.write(json!({"schema_version":1,"future":{"kept":true}}));
    let choices = [
        ("dictation_mic", "true", json!(true)),
        ("panel_tab_icons", "true", json!(true)),
        ("agent_inline_mode", "false", json!(false)),
        ("theme", "native", json!("native")),
        ("ui_text_size", "13", json!(13)),
    ];
    let start = Arc::new(Barrier::new(choices.len() + 1));
    let mut workers = Vec::new();
    for (key, raw, _) in &choices {
        let (sandbox, start, key, raw) = (sandbox.clone(), start.clone(), *key, *raw);
        workers.push(std::thread::spawn(move || {
            start.wait();
            sandbox.ok(&["settings", "set", key, raw]);
        }));
    }
    start.wait();
    SettingsStore::open(sandbox.0.join("state"))
        .unwrap()
        .update(|settings| settings.remember_window_size = false)
        .unwrap();
    for worker in workers {
        worker.join().unwrap();
    }
    let document = sandbox.document();
    for (key, _, expected) in choices {
        assert_eq!(document[key], expected, "{key}");
    }
    assert_eq!(document["remember_window_size"], false);
    assert_eq!(document["future"], json!({"kept":true}));
    sandbox.no_temporary_or_project_files();
}

#[test]
fn help_input_errors_and_literal_option_like_strings() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["settings"],
        vec!["settings", "help"],
        vec!["settings", "--help"],
    ] {
        let help = sandbox.ok(&args);
        assert!(help.contains("top-level"));
        assert!(help.contains("two seconds"));
        assert!(help.contains("ui_text_size"));
    }
    assert!(
        sandbox
            .ok(&["help"])
            .contains("riwork settings set KEY VALUE")
    );
    for args in [
        vec!["settings", "get", "--json"],
        vec!["settings", "set", "theme", "--json"],
        vec!["settings", "list", "extra", "--json"],
        vec!["settings", "set", "theme", "native", "extra", "--json"],
        vec!["settings", "set", "status_bar", "{broken", "--json"],
        vec![
            "settings",
            "set",
            "theme",
            "--json",
            "--json",
            "native",
        ],
    ] {
        sandbox.refused(&args, "invalid_request");
        assert!(!sandbox.file().exists());
    }
    assert_eq!(
        sandbox.ok(&["settings", "set", "selected_codex_account", "--", "--json"]),
        "\"--json\"\n"
    );
    assert_eq!(
        sandbox.json(&["settings", "get", "selected_codex_account", "--json"])["value"],
        "--json"
    );
    assert_eq!(
        sandbox.json(&[
            "settings",
            "set",
            "selected_codex_account",
            "--json",
            "\"true\""
        ])["value"],
        "true"
    );
}
