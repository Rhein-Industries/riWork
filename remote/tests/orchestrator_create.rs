//! `orchestrator.create` (docs/remote-protocol.md, "Orchestrator creation extension"):
//! validation before any CLI runs, the argv it turns into, what it accepts back from
//! the CLI (an entry as `orchestrator list` prints it, plus a boolean `created`), what
//! the phone is shown of it, its errors, and the capability that gates it. Against a
//! stub CLI that records its argv and prints whatever `create.json` holds.
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

struct Fixture {
    _storage_dir: tempfile::TempDir,
    stub: tempfile::TempDir,
    rpc: Rpc,
    device: String,
    project: String,
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
        Self {
            rpc: Rpc::new(stub_cli(stub.path()), storage),
            _storage_dir: storage_dir,
            stub,
            device,
            project: new_uuid(),
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
    async fn create(&self, params: Value) -> Value {
        self.rpc
            .handle(&self.device, req("orchestrator.create", params))
            .await
            .unwrap()
    }
    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.stub.path().join(name), value).unwrap();
    }
    fn cli_says(&self, value: Value) {
        self.set("create.json", &value.to_string());
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
    /// The calls whose first two words are `first second`.
    fn calls_of(&self, first: &str, second: &str) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| {
                c.first().map(String::as_str) == Some(first)
                    && c.get(1).map(String::as_str) == Some(second)
            })
            .collect()
    }
}

/// Logs each call (arguments separated by U+001F). `capabilities` prints
/// `capabilities.out` (default: this CLI can create orchestrators and has chats), or
/// refuses the command if `capabilities.unknown` exists. `project show` prints the id
/// it was asked for, or the one in `show.id`, or fails with the line in `show.error`.
/// `orchestrator create` marks `create.ran`, then prints `create.json`, or fails with
/// the line in `create.error`.
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
             'capabilities --json')\n\
               if [ -e \"$d/capabilities.unknown\" ]; then echo \"riwork: Unknown invocation 'capabilities'\" >&2; exit 2; fi\n\
               if [ -e \"$d/capabilities.out\" ]; then cat \"$d/capabilities.out\"; else printf '{{\"v\":1,\"chat\":true,\"orchestrator_create\":true}}'; fi;;\n\
             'project show')\n\
               if [ -e \"$d/show.error\" ]; then printf 'riwork: %s\\n' \"$(cat \"$d/show.error\")\" >&2; exit 2; fi\n\
               if [ -e \"$d/show.id\" ]; then printf '{{\"id\":\"%s\"}}' \"$(cat \"$d/show.id\")\"; else printf '{{\"id\":\"%s\"}}' \"$3\"; fi;;\n\
             'orchestrator create')\n\
               touch \"$d/create.ran\"\n\
               if [ -e \"$d/create.error\" ]; then printf 'riwork: %s\\n' \"$(cat \"$d/create.error\")\" >&2; exit 2; fi\n\
               cat \"$d/create.json\";;\n\
             esac\n",
            dir = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}

/// An orchestrator as `orchestrator list` prints it, with the CLI-only fields.
fn terminal(id: &str, project: Option<&str>) -> Value {
    json!({
        "id": id,
        "project_id": project,
        "worktree_id": null,
        "kind": "orchestrator",
        "cwd": "/Users/me/code/app",
        "command": "codex --secret-flag",
        "harness": "codex",
        "unrestricted": true,
        "codex_account_id": "acct-secret",
        "alive": true,
        "created_at_unix": 1790000000u64,
        "mode": "terminal"
    })
}
fn chat(id: &str, project: Option<&str>) -> Value {
    json!({
        "id": id,
        "project_id": project,
        "worktree_id": null,
        "kind": "orchestrator",
        "cwd": "/Users/me/code/app/orchestrator",
        "command": null,
        "harness": "claude",
        "unrestricted": false,
        "alive": true,
        "created_at_unix": 1790000000u64,
        "mode": "chat",
        "chat_id": id,
        "provider": "claude",
        "state": "idle",
        "activity": "done",
        "last_activity_unix": 1790000100u64
    })
}
fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}
fn with_created(mut entry: Value, created: Value) -> Value {
    entry["created"] = created;
    entry
}

#[tokio::test]
async fn parameters_are_validated_before_any_cli_runs() {
    let f = Fixture::without_cli();
    let p = f.project.clone();
    let bad = [
        // Not an object.
        json!(null),
        json!([]),
        json!("global"),
        json!(7),
        json!(true),
        // Nothing but a project is a parameter: the Mac decides how it runs.
        json!({"mode":"chat"}),
        json!({"mode":"terminal"}),
        json!({"provider":"codex"}),
        json!({"kind":"orchestrator"}),
        json!({"scope":"global"}),
        json!({"cwd":"/tmp"}),
        json!({"harness":"codex"}),
        json!({"unrestricted":true}),
        json!({"project_id":p,"mode":"chat"}),
        json!({"project_id":p,"worktree_id":new_uuid()}),
        json!({"project_id":p,"json":true}),
        json!({"project_id":p,"owner":f.device}),
        // project_id: a string, a full canonical UUID, never a name, a path or a prefix.
        json!({"project_id":null}),
        json!({"project_id":7}),
        json!({"project_id":true}),
        json!({"project_id":[p]}),
        json!({"project_id":{"id":p}}),
        json!({"project_id":""}),
        json!({"project_id":"app"}),
        json!({"project_id":"/Users/me/code/app"}),
        json!({"project_id":"../.."}),
        json!({"project_id":&p[..8]}),
        json!({"project_id":p.to_uppercase()}),
        json!({"project_id":p.replace('-', "")}),
        json!({"project_id":format!("{p} ")}),
        json!({"project_id":format!("{{{p}}}")}),
        json!({"project_id":"--json"}),
        json!({"project_id":"--project"}),
    ];
    for params in bad {
        let response = f.create(params.clone()).await;
        assert_eq!(response["ok"], false, "{params}: {response}");
        assert_eq!(code(&response), "invalid_request", "{params}: {response}");
    }
}

#[tokio::test]
async fn the_global_orchestrator_is_one_cli_call_with_exactly_these_arguments() {
    let f = Fixture::new();
    let id = new_uuid();
    f.cli_says(with_created(terminal(&id, None), json!(true)));
    let response = f.create(json!({})).await;
    assert_eq!(response["ok"], true, "{response}");
    // The question about what the CLI can do, then the one call: nothing to look up for
    // the global scope, and no flag for the mode, which the desktop's setting decides.
    assert_eq!(
        f.calls(),
        [
            argv(&["capabilities", "--json"]),
            argv(&["orchestrator", "create", "--json"])
        ]
    );
}

#[tokio::test]
async fn a_project_orchestrator_checks_the_project_by_exact_id_and_passes_it_as_one_argument() {
    let f = Fixture::new();
    let id = new_uuid();
    f.cli_says(with_created(chat(&id, Some(&f.project)), json!(true)));
    let response = f.create(json!({"project_id": f.project})).await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        f.calls(),
        [
            argv(&["capabilities", "--json"]),
            argv(&["project", "show", &f.project, "--json"]),
            argv(&["orchestrator", "create", "--project", &f.project, "--json"])
        ]
    );
}

#[tokio::test]
async fn an_unknown_project_is_not_found_and_nothing_is_created() {
    let f = Fixture::new();
    f.cli_says(with_created(terminal(&new_uuid(), None), json!(true)));
    // The CLI also resolves names and prefixes: an id that is somebody else's is refused.
    f.set("show.id", &new_uuid());
    let response = f.create(json!({"project_id": f.project})).await;
    assert_eq!(code(&response), "not_found", "{response}");
    assert_eq!(message(&response), "project not found on the desktop");
    // And one the CLI does not know at all.
    std::fs::remove_file(f.stub.path().join("show.id")).unwrap();
    f.set("show.error", &format!("No project matches '{}'", f.project));
    let response = f.create(json!({"project_id": f.project})).await;
    assert_eq!(code(&response), "not_found", "{response}");
    assert_eq!(message(&response), "project not found on the desktop");
    assert!(
        f.calls_of("orchestrator", "create").is_empty(),
        "{:?}",
        f.calls()
    );
}

#[tokio::test]
async fn a_new_orchestrator_is_created_and_an_existing_one_is_returned_as_it_is() {
    let f = Fixture::new();
    let (id, project_id) = (new_uuid(), f.project.clone());
    // Made now, as a terminal.
    f.cli_says(with_created(terminal(&id, Some(&project_id)), json!(true)));
    let made = f.create(json!({"project_id": project_id})).await;
    assert_eq!(made["ok"], true, "{made}");
    assert_eq!(made["result"]["created"], true);
    assert_eq!(
        made["result"]["orchestrator"],
        json!({
            "id": id, "project_id": project_id, "worktree_id": null, "kind": "orchestrator",
            "cwd": "/Users/me/code/app", "harness": "codex", "alive": true,
            "created_at_unix": 1790000000u64, "mode": "terminal"
        })
    );
    // Already there, as a chat: the same entry, `created` false.
    let existing = chat(&id, Some(&project_id));
    f.cli_says(with_created(existing, json!(false)));
    let again = f.create(json!({"project_id": project_id})).await;
    assert_eq!(again["result"]["created"], false, "{again}");
    assert_eq!(
        again["result"]["orchestrator"],
        json!({
            "id": id, "project_id": project_id, "worktree_id": null, "kind": "orchestrator",
            "cwd": "/Users/me/code/app/orchestrator", "harness": "claude", "alive": true,
            "created_at_unix": 1790000000u64, "last_activity_unix": 1790000100u64,
            "activity": "done", "mode": "chat", "chat_id": id, "provider": "claude"
        })
    );
    // The result has exactly these two keys, and `created` is not in the entry.
    let keys: Vec<&String> = again["result"].as_object().unwrap().keys().collect();
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert!(again["result"]["orchestrator"].get("created").is_none());
    assert!(again["result"]["orchestrator"].get("state").is_none());
}

#[tokio::test]
async fn the_global_orchestrator_as_a_chat_is_projected_like_a_list_entry() {
    let f = Fixture::new();
    let id = new_uuid();
    f.cli_says(with_created(chat(&id, None), json!(true)));
    let made = f.create(json!({})).await;
    assert_eq!(made["ok"], true, "{made}");
    let entry = &made["result"]["orchestrator"];
    assert_eq!(entry["project_id"], Value::Null);
    assert_eq!(entry["mode"], "chat");
    assert_eq!(entry["chat_id"], id);
    assert_eq!(entry["id"], entry["chat_id"]);
    assert_eq!(entry["provider"], "claude");
    let text = made.to_string();
    for private in ["secret-flag", "acct-secret", "unrestricted"] {
        assert!(!text.contains(private), "{private} leaked: {text}");
    }
}

#[tokio::test]
async fn a_malformed_mode_provider_or_chat_id_is_left_out_of_the_entry() {
    let f = Fixture::new();
    let id = new_uuid();
    let cases = [
        ("mode", json!("other"), vec!["mode", "chat_id", "provider"]),
        ("mode", json!(null), vec!["mode", "chat_id", "provider"]),
        ("provider", json!("grok"), vec!["provider"]),
        ("provider", json!("Claude"), vec!["provider"]),
        ("chat_id", json!("not-a-uuid"), vec!["chat_id"]),
        ("chat_id", json!(id.to_uppercase()), vec!["chat_id"]),
        ("chat_id", json!(7), vec!["chat_id"]),
    ];
    for (field, wrong, left_out) in cases {
        let mut entry = chat(&id, None);
        entry[field] = wrong.clone();
        f.cli_says(with_created(entry, json!(true)));
        let made = f.create(json!({})).await;
        assert_eq!(made["ok"], true, "{field}={wrong}: {made}");
        assert_eq!(made["result"]["created"], true, "{field}={wrong}");
        let shown = &made["result"]["orchestrator"];
        for gone in left_out {
            assert!(shown.get(gone).is_none(), "{field}={wrong}: {gone} passed");
        }
        assert_eq!(shown["id"], id, "{field}={wrong}");
    }
    // A terminal entry that claims a chat has none of it passed on.
    let mut entry = terminal(&id, None);
    entry["chat_id"] = json!(id);
    entry["provider"] = json!("codex");
    f.cli_says(with_created(entry, json!(false)));
    let shown = f.create(json!({})).await["result"]["orchestrator"].clone();
    assert_eq!(shown["mode"], "terminal");
    assert!(shown.get("chat_id").is_none() && shown.get("provider").is_none());
    // An older CLI that does not say the mode at all: the entry as it was.
    let mut entry = terminal(&id, None);
    entry.as_object_mut().unwrap().remove("mode");
    f.cli_says(with_created(entry, json!(true)));
    let shown = f.create(json!({})).await["result"]["orchestrator"].clone();
    assert!(shown.get("mode").is_none(), "{shown}");
}

#[tokio::test]
async fn a_created_that_is_not_a_boolean_is_never_guessed() {
    let f = Fixture::new();
    let id = new_uuid();
    for wrong in [
        Some(json!("true")),
        Some(json!(1)),
        Some(json!(0)),
        Some(json!(null)),
        Some(json!({"created": true})),
        None,
    ] {
        let mut entry = terminal(&id, None);
        if let Some(wrong) = &wrong {
            entry["created"] = wrong.clone();
        }
        f.cli_says(entry);
        let response = f.create(json!({})).await;
        assert_eq!(code(&response), "cli_error", "{wrong:?}: {response}");
        assert!(
            message(&response).contains("whether the orchestrator was created"),
            "{wrong:?}: {response}"
        );
    }
}

#[tokio::test]
async fn an_orchestrator_of_another_scope_or_shape_is_not_passed_on() {
    let f = Fixture::new();
    let (id, other) = (new_uuid(), new_uuid());
    let global_asked = [
        // A project's orchestrator for the global scope.
        terminal(&id, Some(&other)),
        // Not an orchestrator.
        {
            let mut entry = terminal(&id, None);
            entry["kind"] = json!("project");
            entry
        },
        // Not an id.
        terminal("not-a-uuid", None),
        terminal(&id.to_uppercase(), None),
        // Without what a phone needs of a session.
        {
            let mut entry = terminal(&id, None);
            entry.as_object_mut().unwrap().remove("cwd");
            entry
        },
        {
            let mut entry = terminal(&id, None);
            entry["alive"] = json!("yes");
            entry
        },
        {
            let mut entry = terminal(&id, None);
            entry["created_at_unix"] = json!(-1);
            entry
        },
    ];
    for entry in global_asked {
        f.cli_says(with_created(entry.clone(), json!(true)));
        let response = f.create(json!({})).await;
        assert_eq!(code(&response), "cli_error", "{entry}: {response}");
        assert_eq!(
            message(&response),
            "CLI returned an orchestrator that does not match the request",
            "{entry}"
        );
    }
    // The global one, or another project's, for a project.
    for entry in [terminal(&id, None), terminal(&id, Some(&other))] {
        f.cli_says(with_created(entry.clone(), json!(false)));
        let response = f.create(json!({"project_id": f.project})).await;
        assert_eq!(code(&response), "cli_error", "{entry}: {response}");
    }
}

#[tokio::test]
async fn a_cli_failure_surfaces_as_the_usual_fault() {
    let f = Fixture::new();
    for (line, expected_code, expected_message) in [
        (
            "boom: the registry is locked",
            "cli_error",
            "boom: the registry is locked",
        ),
        (
            "codex is not installed or is not on PATH",
            "harness_unavailable",
            "codex is not installed or is not on PATH",
        ),
        (
            "No project matches 'x'",
            "not_found",
            "project not found on the desktop",
        ),
    ] {
        f.set("create.error", line);
        let response = f.create(json!({})).await;
        assert_eq!(response["ok"], false, "{line}: {response}");
        assert_eq!(code(&response), expected_code, "{line}: {response}");
        assert_eq!(message(&response), expected_message, "{line}");
    }
    // Output that is not JSON at all is the CLI's fault too.
    std::fs::remove_file(f.stub.path().join("create.error")).unwrap();
    f.set("create.json", "created");
    let response = f.create(json!({})).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    // And so is JSON that is not an entry.
    f.set("create.json", "[]");
    let response = f.create(json!({})).await;
    assert_eq!(code(&response), "cli_error", "{response}");
}

#[tokio::test]
async fn a_revoked_device_creates_nothing() {
    let f = Fixture::new();
    f.cli_says(with_created(terminal(&new_uuid(), None), json!(true)));
    f.rpc.storage.revoke(&f.device).unwrap();
    assert!(
        f.rpc
            .handle(&f.device, req("orchestrator.create", json!({})))
            .await
            .is_err()
    );
    assert!(f.calls().is_empty(), "{:?}", f.calls());
}

#[tokio::test]
async fn a_cli_that_cannot_create_orchestrators_is_not_asked_to() {
    // Says no, says nothing of it, answers it oddly, refuses the question, or prints
    // nothing at all.
    for answer in [
        Some("{\"v\":1,\"orchestrator_create\":false}"),
        Some("{\"v\":1,\"chat\":true}"),
        Some("{\"v\":1,\"orchestrator_create\":\"true\"}"),
        Some("{\"v\":2,\"orchestrator_create\":true}"),
        Some("orchestrator_create yes"),
        Some(""),
        None,
    ] {
        let f = Fixture::new();
        match answer {
            Some(text) => f.set("capabilities.out", text),
            None => f.set("capabilities.unknown", ""),
        }
        f.cli_says(with_created(terminal(&new_uuid(), None), json!(true)));
        assert!(
            !f.rpc.orchestrator_create_supported().await,
            "{answer:?} was believed"
        );
        let response = f.create(json!({})).await;
        assert_eq!(code(&response), "cli_error", "{answer:?}: {response}");
        assert!(
            message(&response).contains("update RiWork"),
            "{answer:?}: {response}"
        );
        assert!(
            f.calls_of("orchestrator", "create").is_empty(),
            "{answer:?}: {:?}",
            f.calls()
        );
    }
}

#[tokio::test]
async fn a_yes_is_remembered_and_one_answer_serves_chats_and_orchestrators() {
    let f = Fixture::new();
    f.cli_says(with_created(terminal(&new_uuid(), None), json!(true)));
    assert!(f.rpc.orchestrator_create_supported().await);
    // The same answer said chats are there: the CLI is not asked again for them.
    assert!(f.rpc.chat_supported().await);
    assert!(f.rpc.orchestrator_create_supported().await);
    assert_eq!(f.create(json!({})).await["ok"], true);
    assert_eq!(f.calls_of("capabilities", "--json").len(), 1);

    // A CLI that has orchestrator creation but not chats is believed for what it says.
    let f = Fixture::new();
    f.set("capabilities.out", "{\"v\":1,\"orchestrator_create\":true}");
    assert!(f.rpc.orchestrator_create_supported().await);
    assert!(!f.rpc.chat_supported().await);
    // And a CLI updated while the connector runs is believed at once.
    f.set(
        "capabilities.out",
        "{\"v\":1,\"orchestrator_create\":true,\"chat\":true}",
    );
    assert!(f.rpc.chat_supported().await);
}
