//! `project.memo.get` and `project.memo.set`: validation before any CLI runs, the argv they turn
//! into (the text of a save goes on standard input, never in argv), what is passed back of the
//! CLI's reply, its refusals by code, and the capability that gates both, against a stub CLI that
//! records its argv and standard input. The one test against the real CLI is ignored and runs in a
//! throwaway RIWORK_HOME and HOME.
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
    storage_dir: tempfile::TempDir,
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
            storage_dir,
            stub,
            device,
        }
    }
    fn without_cli() -> Self {
        let mut fixture = Self::new();
        fixture.rpc = Rpc::new(
            "/nonexistent/no-CLI-may-be-executed".into(),
            fixture.rpc.storage.clone(),
        );
        fixture
    }
    async fn call(&self, method: &str, params: Value) -> Value {
        self.rpc
            .handle(&self.device, req(method, params))
            .await
            .unwrap()
    }
    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.stub.path().join(name), value).unwrap();
    }
    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.stub.path().join(name)).unwrap_or_default()
    }
    /// Every argv the stub was run with, each argument separately.
    fn calls(&self) -> Vec<Vec<String>> {
        self.read("argv.log")
            .lines()
            .map(|line| {
                line.split('\u{1f}')
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .collect()
    }
}

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
             case \"$1 $2 $3\" in\n\
             'capabilities --json ')\n\
               if [ -e \"$d/capabilities.out\" ]; then cat \"$d/capabilities.out\"; else printf '{{\"v\":1,\"project_memo\":true}}'; fi;;\n\
             'project show '*)\n\
               if [ -e \"$d/project.json\" ]; then cat \"$d/project.json\"; else printf '{{\"id\":\"%s\"}}' \"$3\"; fi;;\n\
             'project memo show') cat \"$d/show.json\";;\n\
             'project memo set') cat > \"$d/stdin.log\"; cat \"$d/set.json\";;\n\
             esac\n",
            dir = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}

#[test]
fn the_connectors_cap_is_the_stores() {
    assert_eq!(
        riwork_remote::rpc::MEMO_MAX_BYTES,
        riwork_core::project_memo::MAX_BYTES
    );
}

#[tokio::test]
async fn parameters_are_validated_before_any_cli_runs() {
    let f = Fixture::without_cli();
    let project = new_uuid();
    let over = "x".repeat(riwork_remote::rpc::MEMO_MAX_BYTES + 1);
    for (method, params) in [
        ("project.memo.get", json!(null)),
        ("project.memo.get", json!([])),
        ("project.memo.get", json!({})),
        ("project.memo.get", json!({"project_id": "Main"})),
        (
            "project.memo.get",
            json!({"project_id": project, "all": true}),
        ),
        ("project.memo.set", json!({"project_id": project})),
        (
            "project.memo.set",
            json!({"project_id": project, "text": null}),
        ),
        (
            "project.memo.set",
            json!({"project_id": project, "text": ["x"]}),
        ),
        ("project.memo.set", json!({"project_id": "x", "text": "x"})),
        (
            "project.memo.set",
            json!({"project_id": project, "text": "x", "base_updated_at": 1}),
        ),
        (
            "project.memo.set",
            json!({"project_id": project, "text": over}),
        ),
    ] {
        let response = f.call(method, params.clone()).await;
        assert_eq!(
            code(&response),
            "invalid_request",
            "{method} {params}: {response}"
        );
    }
    // Valid params reach the CLI, which cannot be run here.
    let response = f
        .call("project.memo.get", json!({"project_id": project}))
        .await;
    assert_eq!(code(&response), "cli_error", "{response}");
}

#[tokio::test]
async fn without_the_capability_both_methods_are_unsupported() {
    for answer in [
        "{\"v\":1,\"dashboard\":true}",
        "{\"v\":1,\"project_memo\":\"true\"}",
        "{\"v\":2,\"project_memo\":true}",
        "project_memo yes",
    ] {
        let f = Fixture::new();
        f.set("capabilities.out", answer);
        assert!(!f.rpc.project_memo_supported().await, "{answer}");
        for (method, params) in [
            ("project.memo.get", json!({"project_id": new_uuid()})),
            (
                "project.memo.set",
                json!({"project_id": new_uuid(), "text": "x"}),
            ),
        ] {
            let response = f.call(method, params).await;
            assert_eq!(code(&response), "invalid_request", "{answer}: {response}");
            assert_eq!(message(&response), "unsupported RPC method", "{answer}");
        }
        assert!(
            f.calls().iter().all(|argv| argv[0] == "capabilities"),
            "{answer}: {:?}",
            f.calls()
        );
    }
    let f = Fixture::new();
    assert!(f.rpc.project_memo_supported().await);
}

#[tokio::test]
async fn get_reads_the_memo_and_passes_only_its_fields() {
    let f = Fixture::new();
    let project = new_uuid();
    f.set(
        "show.json",
        &json!({"v":1,"project_id":project,"text":"# Notes\n- a","updated_at":1790000000,"root":"/Users/me"})
            .to_string(),
    );
    let response = f
        .call("project.memo.get", json!({"project_id": project}))
        .await;
    assert_eq!(
        response["result"],
        json!({"project_id": project, "text": "# Notes\n- a", "updated_at": 1790000000}),
        "{response}"
    );
    assert_eq!(
        f.calls().last().unwrap(),
        &["project", "memo", "show", project.as_str(), "--json"]
    );

    // A memo never saved has no time; a refusal keeps its code.
    f.set(
        "show.json",
        &json!({"v":1,"project_id":project,"text":""}).to_string(),
    );
    let response = f
        .call("project.memo.get", json!({"project_id": project}))
        .await;
    assert_eq!(
        response["result"],
        json!({"project_id": project, "text": ""})
    );
    f.set(
        "show.json",
        r#"{"error":{"code":"not_found","message":"No project matches it"}}"#,
    );
    let response = f
        .call("project.memo.get", json!({"project_id": project}))
        .await;
    assert_eq!(code(&response), "not_found");
    assert_eq!(message(&response), "No project matches it");
}

#[tokio::test]
async fn set_sends_the_text_on_stdin_and_never_in_argv() {
    let f = Fixture::new();
    let project = new_uuid();
    let text = "# Plan $(touch pwned) `id`\n- \"quoted\" & 'single'\n\u{1f600}";
    f.set(
        "set.json",
        &json!({"v":1,"project_id":project,"text":text,"updated_at":7}).to_string(),
    );
    let response = f
        .call(
            "project.memo.set",
            json!({"project_id": project, "text": text}),
        )
        .await;
    assert_eq!(
        response["result"],
        json!({"project_id": project, "text": text, "updated_at": 7}),
        "{response}"
    );
    assert_eq!(f.read("stdin.log"), text);
    let argv = f.calls().last().unwrap().clone();
    assert_eq!(argv, ["project", "memo", "set", project.as_str(), "--json"]);
    assert!(!f.stub.path().join("pwned").exists());

    // An empty memo is a save like any other.
    f.set(
        "set.json",
        &json!({"v":1,"project_id":project,"text":"","updated_at":8}).to_string(),
    );
    let response = f
        .call(
            "project.memo.set",
            json!({"project_id": project, "text": ""}),
        )
        .await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(f.read("stdin.log"), "");

    // The CLI's own refusal, and a reply about another project, are errors, not a saved memo.
    f.set(
        "set.json",
        r#"{"error":{"code":"invalid_request","message":"The memo is too long"}}"#,
    );
    let response = f
        .call(
            "project.memo.set",
            json!({"project_id": project, "text": "x"}),
        )
        .await;
    assert_eq!(code(&response), "invalid_request");
    f.set(
        "set.json",
        &json!({"v":1,"project_id":new_uuid(),"text":"x"}).to_string(),
    );
    let response = f
        .call(
            "project.memo.set",
            json!({"project_id": project, "text": "x"}),
        )
        .await;
    assert_eq!(code(&response), "cli_error");
}

#[tokio::test]
async fn a_save_writes_only_a_project_with_exactly_that_id() {
    let f = Fixture::new();
    let project = new_uuid();
    // The CLI also matches names: an id the desktop does not have, which another project is
    // named, resolves to that other project.
    let other = new_uuid();
    f.set(
        "project.json",
        &json!({"id": other, "name": project}).to_string(),
    );
    f.set(
        "set.json",
        &json!({"v":1,"project_id":other,"text":"x","updated_at":7}).to_string(),
    );
    let response = f
        .call(
            "project.memo.set",
            json!({"project_id": project, "text": "x"}),
        )
        .await;
    let saves = |f: &Fixture| {
        f.calls()
            .iter()
            .filter(|argv| argv.starts_with(&["project".into(), "memo".into(), "set".into()]))
            .count()
    };
    assert_eq!(saves(&f), 0, "{:?}", f.calls());
    assert_eq!(code(&response), "not_found", "{response}");
    assert!(
        f.calls().contains(&vec![
            "project".into(),
            "show".into(),
            project.clone(),
            "--json".into()
        ]),
        "{:?}",
        f.calls()
    );

    // The project the phone named is the one saved.
    f.set("project.json", &json!({"id": project}).to_string());
    f.set(
        "set.json",
        &json!({"v":1,"project_id":project,"text":"x","updated_at":7}).to_string(),
    );
    let response = f
        .call(
            "project.memo.set",
            json!({"project_id": project, "text": "x"}),
        )
        .await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(saves(&f), 1);
}

#[tokio::test]
async fn a_revoked_device_saves_nothing() {
    let f = Fixture::new();
    let project = new_uuid();
    f.set(
        "set.json",
        &json!({"v":1,"project_id":project,"text":"x"}).to_string(),
    );
    let storage = Storage::at(f.storage_dir.path().into()).unwrap();
    storage.revoke(&f.device).unwrap();
    let response = f
        .rpc
        .handle(
            &f.device,
            req(
                "project.memo.set",
                json!({"project_id": project, "text": "x"}),
            ),
        )
        .await;
    match response {
        Ok(response) => assert_ne!(response["ok"], true, "{response}"),
        Err(error) => assert!(error.to_string().contains("revoked"), "{error}"),
    }
    assert!(
        !f.calls()
            .iter()
            .any(|argv| argv.first().map(String::as_str) == Some("project"))
    );
}

/// The real CLI, in a throwaway RIWORK_HOME and HOME, behind a wrapper.
struct RealCli {
    dir: tempfile::TempDir,
    wrapper: PathBuf,
}
impl RealCli {
    fn new(cli: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let (home, state) = (base.join("home"), base.join("state"));
        std::fs::create_dir_all(&home).unwrap();
        let wrapper = base.join("riwork");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexport HOME='{home}' RIWORK_HOME='{state}' RIWORK_RUNTIME_DIR='{state}/runtime'\n\
                 cd '{home}' || exit 1\nexec '{cli}' \"$@\"\n",
                home = home.display(),
                state = state.display(),
                cli = cli.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, wrapper }
    }
    fn state(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap().join("state")
    }
}

#[tokio::test]
#[ignore = "requires RIWORK_TEST_CLI; real CLI in a throwaway HOME and RIWORK_HOME"]
async fn the_real_cli_round_trips_a_memo_through_the_rpc() {
    let cli = PathBuf::from(std::env::var_os("RIWORK_TEST_CLI").expect("set RIWORK_TEST_CLI"));
    assert!(cli.is_absolute(), "absolute CLI path required");
    let real = RealCli::new(&cli);
    let store = riwork_core::store::Store::open(real.state()).unwrap();
    let root = real.dir.path().canonicalize().unwrap().join("project");
    std::fs::create_dir_all(&root).unwrap();
    let project = store.add_project(&root, Some("Memo")).unwrap();
    let other = store
        .add_project(
            real.dir.path().canonicalize().unwrap().join("home"),
            Some("Other"),
        )
        .unwrap();
    store.set_project_memo(&other.id, "other").unwrap();

    let f = Fixture::new();
    let rpc = Rpc::new(real.wrapper.clone(), f.rpc.storage.clone());
    let call = |method: &str, params: Value| {
        let (rpc, device) = (&rpc, f.device.clone());
        let request = req(method, params);
        async move { rpc.handle(&device, request).await.unwrap() }
    };
    let empty = call("project.memo.get", json!({"project_id": project.id})).await;
    assert_eq!(
        empty["result"],
        json!({"project_id": project.id, "text": ""}),
        "{empty}"
    );

    let text = "# From the phone\r\n- \u{1f600} $(no)\n";
    let saved = call(
        "project.memo.set",
        json!({"project_id": project.id, "text": text}),
    )
    .await;
    assert_eq!(
        saved["result"]["text"], "# From the phone\n- \u{1f600} $(no)\n",
        "{saved}"
    );
    assert!(saved["result"]["updated_at"].as_u64().unwrap() > 0);
    let read = call("project.memo.get", json!({"project_id": project.id})).await;
    assert_eq!(read["result"], saved["result"]);
    // The desktop's store sees the phone's save, and nothing else changed.
    let state = store.snapshot().unwrap();
    assert_eq!(
        state.project_memo(&project.id).text,
        "# From the phone\n- \u{1f600} $(no)\n"
    );
    assert_eq!(state.project_memo(&other.id).text, "other");

    let missing = call("project.memo.get", json!({"project_id": new_uuid()})).await;
    assert_eq!(code(&missing), "not_found", "{missing}");
    let missing = call(
        "project.memo.set",
        json!({"project_id": new_uuid(), "text": "x"}),
    )
    .await;
    assert_eq!(code(&missing), "not_found", "{missing}");

    // An id the desktop does not have, which another project is named: that project's memo is
    // left alone.
    let absent = new_uuid();
    store.rename_project(&other.id, &absent).unwrap();
    let refused = call(
        "project.memo.set",
        json!({"project_id": absent, "text": "not for Other"}),
    )
    .await;
    assert_eq!(
        store.snapshot().unwrap().project_memo(&other.id).text,
        "other"
    );
    assert_eq!(code(&refused), "not_found", "{refused}");
}
