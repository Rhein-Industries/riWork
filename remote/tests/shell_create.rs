//! `shell.create` and `shell.close`: validation before any CLI runs, the argv
//! they turn into (one argument per value, never a shell string), what they
//! accept back from the CLI, and their errors, against a stub CLI that records
//! its argv and prints whatever `create.json` holds.
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

struct Fixture {
    _storage_dir: tempfile::TempDir,
    stub: tempfile::TempDir,
    rpc: Rpc,
    device: String,
    project: String,
    worktree: String,
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
            project: new_uuid(),
            worktree: new_uuid(),
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
    async fn create(&self, params: Value) -> Value {
        self.call(req("shell.create", params)).await
    }
    async fn close(&self, shell: &str) -> Value {
        self.call(req("shell.close", json!({ "shell_id": shell })))
            .await
    }
    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.stub.path().join(name), value).unwrap();
    }
    fn cli_says(&self, value: Value) {
        self.set("create.json", &value.to_string());
    }
    /// The CLI fails `shell create` with this one line on stderr.
    fn cli_fails(&self, line: &str) {
        self.set("create.error", line);
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
                c.first().is_some_and(|w| w == first) && c.get(1).is_some_and(|w| w == second)
            })
            .collect()
    }
    /// What the CLI prints for a session it just made.
    fn session(&self, id: &str, harness: Value) -> Value {
        json!({
            "id": id,
            "project_id": self.project,
            "worktree_id": self.worktree,
            "kind": "project",
            "cwd": "/Users/me/code/app",
            "command": null,
            "editor_path": null,
            "harness": harness,
            "unrestricted": false,
            "codex_account_id": "acct",
            "codex_account_label": "Work",
            "codex_account_email": "me@example.com",
            "codex_home": "/Users/me/.codex",
            "orchestrator_skill_loaded": false,
            "orchestrator_skill_version": null,
            "orchestrator_project_root": null,
            "created_at_unix": 1790000000u64,
            "alive": true
        })
    }
}

/// Logs each call (arguments separated by U+001F). `capabilities --json` prints
/// `capabilities.json`, or nothing, like a CLI from before the question. `project show` and
/// `worktree show` print the id they were asked for, or the one in `show.id`,
/// or fail with the line in `show.error`. `shell create` waits for `create.gate` (20 s at
/// most) if `create.hold` exists, marks `create.ran`, then prints
/// `create.json`, or fails with the line in `create.error`. `shell list` and
/// `orchestrator list` print `shells.json` and `orchestrators.json` (default
/// `[]`). `shell close` fails like a CLI that does not know the shell if
/// `close.error` exists. `shell resize` and `resize-clear` succeed.
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
             'capabilities --json') if [ -e \"$d/capabilities.json\" ]; then cat \"$d/capabilities.json\"; fi;;\n\
             'project show'|'worktree show')\n\
               if [ -e \"$d/show.error\" ]; then printf 'riwork: %s\\n' \"$(cat \"$d/show.error\")\" >&2; exit 2; fi\n\
               if [ -e \"$d/show.id\" ]; then printf '{{\"id\":\"%s\"}}' \"$(cat \"$d/show.id\")\"; else printf '{{\"id\":\"%s\"}}' \"$3\"; fi;;\n\
             'shell create')\n\
               if [ -e \"$d/create.hold\" ]; then i=0; while [ ! -e \"$d/create.gate\" ] && [ $i -lt 400 ]; do sleep 0.05; i=$((i+1)); done; fi\n\
               touch \"$d/create.ran\"\n\
               if [ -e \"$d/create.error\" ]; then printf 'riwork: %s\\n' \"$(cat \"$d/create.error\")\" >&2; exit 2; fi\n\
               cat \"$d/create.json\";;\n\
             'shell list') if [ -e \"$d/shells.json\" ]; then cat \"$d/shells.json\"; else echo '[]'; fi;;\n\
             'orchestrator list') if [ -e \"$d/orchestrators.json\" ]; then cat \"$d/orchestrators.json\"; else echo '[]'; fi;;\n\
             'shell close')\n\
               if [ -e \"$d/close.error\" ]; then printf 'riwork: %s\\n' \"$(cat \"$d/close.error\")\" >&2; exit 2; fi;;\n\
             esac\n",
            dir = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}

#[tokio::test]
async fn parameters_are_validated_before_any_cli_runs() {
    let f = Fixture::without_cli();
    let (p, w) = (f.project.clone(), f.worktree.clone());
    let long = "x".repeat(4097);
    let bad = [
        // Not an object.
        json!(null),
        json!([]),
        json!("shell"),
        json!(7),
        json!({}),
        // Exactly one target.
        json!({"kind":"shell"}),
        json!({"project_id":p,"worktree_id":w,"kind":"shell"}),
        json!({"project_id":null,"kind":"shell"}),
        json!({"worktree_id":null,"kind":"shell"}),
        json!({"project_id":p,"worktree_id":null,"kind":"shell"}),
        // Targets are full canonical UUIDs, never names, paths or prefixes.
        json!({"project_id":"","kind":"shell"}),
        json!({"project_id":"app","kind":"shell"}),
        json!({"project_id":"/Users/me/code/app","kind":"shell"}),
        json!({"project_id":"../..","kind":"shell"}),
        json!({"project_id":&p[..8],"kind":"shell"}),
        json!({"project_id":p.to_uppercase(),"kind":"shell"}),
        json!({"project_id":p.replace('-', ""),"kind":"shell"}),
        json!({"project_id":format!("{p} "),"kind":"shell"}),
        json!({"project_id":"--worktree","kind":"shell"}),
        json!({"project_id":7,"kind":"shell"}),
        json!({"project_id":[p],"kind":"shell"}),
        json!({"worktree_id":"main","kind":"shell"}),
        json!({"worktree_id":w.to_uppercase(),"kind":"shell"}),
        json!({"worktree_id":&w[..8],"kind":"shell"}),
        json!({"worktree_id":true,"kind":"shell"}),
        // The kind is required and one of four names, spelled exactly.
        json!({"project_id":p}),
        json!({"project_id":p,"kind":null}),
        json!({"project_id":p,"kind":7}),
        json!({"project_id":p,"kind":""}),
        json!({"project_id":p,"kind":"Shell"}),
        json!({"project_id":p,"kind":"CODEX"}),
        json!({"project_id":p,"kind":"bash"}),
        json!({"project_id":p,"kind":"zsh"}),
        json!({"project_id":p,"kind":"orchestrator"}),
        json!({"project_id":p,"kind":"codex "}),
        json!({"project_id":p,"kind":["codex"]}),
        // unrestricted: a boolean, and only for the agents.
        json!({"project_id":p,"kind":"codex","unrestricted":"true"}),
        json!({"project_id":p,"kind":"codex","unrestricted":1}),
        json!({"project_id":p,"kind":"codex","unrestricted":null}),
        json!({"project_id":p,"kind":"shell","unrestricted":true}),
        // as_settings: a boolean, only for the agents, and never with unrestricted.
        json!({"project_id":p,"kind":"codex","as_settings":"true"}),
        json!({"project_id":p,"kind":"codex","as_settings":null}),
        json!({"project_id":p,"kind":"shell","as_settings":true}),
        json!({"project_id":p,"kind":"codex","as_settings":true,"unrestricted":false}),
        // command: a plain shell's only.
        json!({"project_id":p,"kind":"codex","command":"ls"}),
        json!({"project_id":p,"kind":"claude","command":"ls"}),
        json!({"project_id":p,"kind":"grok","command":"ls"}),
        json!({"project_id":p,"kind":"shell","command":null}),
        json!({"project_id":p,"kind":"shell","command":7}),
        json!({"project_id":p,"kind":"shell","command":["ls"]}),
        json!({"project_id":p,"kind":"shell","command":""}),
        json!({"project_id":p,"kind":"shell","command":"   "}),
        json!({"project_id":p,"kind":"shell","command":"\u{a0}"}),
        json!({"project_id":p,"kind":"shell","command":long}),
        json!({"project_id":p,"kind":"shell","command":"ls\nrm -rf ~"}),
        json!({"project_id":p,"kind":"shell","command":"ls\r"}),
        json!({"project_id":p,"kind":"shell","command":"ls\tx"}),
        json!({"project_id":p,"kind":"shell","command":"a\u{0}b"}),
        json!({"project_id":p,"kind":"shell","command":"a\u{1b}[2Jb"}),
        json!({"project_id":p,"kind":"shell","command":"a\u{7f}b"}),
        json!({"project_id":p,"kind":"shell","command":"a\u{85}b"}),
        json!({"project_id":p,"kind":"shell","command":"a\u{2028}b"}),
        json!({"project_id":p,"kind":"shell","command":"a\u{2029}b"}),
        // The CLI would read these as options of its own.
        json!({"project_id":p,"kind":"shell","command":"--json"}),
        json!({"project_id":p,"kind":"shell","command":"--harness"}),
        json!({"project_id":p,"kind":"shell","command":"--project"}),
        json!({"project_id":p,"kind":"shell","command":"--command"}),
        json!({"project_id":p,"kind":"shell","command":"-x"}),
        json!({"project_id":p,"kind":"shell","command":"-"}),
        // Nothing else is a parameter: the Mac decides where and how it runs.
        json!({"project_id":p,"kind":"shell","cwd":"/tmp"}),
        json!({"project_id":p,"kind":"shell","path":"/tmp"}),
        json!({"project_id":p,"kind":"shell","name":"x"}),
        json!({"project_id":p,"kind":"shell","args":["a"]}),
        json!({"project_id":p,"kind":"shell","env":{"A":"b"}}),
        json!({"project_id":p,"kind":"shell","shell":"/bin/zsh"}),
        json!({"project_id":p,"kind":"codex","harness":"codex"}),
        json!({"project_id":p,"kind":"codex","account":"x"}),
        json!({"project_id":p,"kind":"shell","json":true}),
        json!({"project_id":p,"kind":"shell","owner":f.device}),
        json!({"project_id":p,"kind":"shell","wait_ms":10}),
    ];
    for params in bad {
        let response = f.create(params.clone()).await;
        assert_eq!(code(&response), "invalid_request", "{params}: {response}");
    }
    // The size limit is in bytes, not characters.
    let wide = "\u{e9}".repeat(2049);
    assert!(wide.chars().count() < 4096 && wide.len() > 4096);
    let response = f
        .create(json!({"project_id":p,"kind":"shell","command":wide}))
        .await;
    assert_eq!(code(&response), "invalid_request", "{response}");
}

#[tokio::test]
async fn close_parameters_are_validated_before_any_cli_runs() {
    let f = Fixture::without_cli();
    let shell = new_uuid();
    for params in [
        json!(null),
        json!([]),
        json!({}),
        json!({"shell_id":null}),
        json!({"shell_id":7}),
        json!({"shell_id":""}),
        json!({"shell_id":"nope"}),
        json!({"shell_id":&shell[..8]}),
        json!({"shell_id":shell.to_uppercase()}),
        json!({"shell_id":shell.replace('-', "")}),
        json!({"shell_id":format!("{shell} ")}),
        json!({"shell_id":shell,"force":true}),
        json!({"shell_id":shell,"kind":"project"}),
        json!({"shell_id":shell,"project_id":f.project}),
    ] {
        let response = f.call(req("shell.close", params.clone())).await;
        assert_eq!(code(&response), "invalid_request", "{params}: {response}");
    }
}

#[tokio::test]
async fn every_value_is_its_own_argument_and_the_cli_gets_json() {
    let f = Fixture::new();
    let (p, w) = (f.project.clone(), f.worktree.clone());
    let shell = new_uuid();
    f.cli_says(f.session(&shell, Value::Null));
    // The cases: what the phone sends, what the CLI is run with. `--json` is
    // always last, whatever came before.
    let hostile =
        "echo \"hi\" ; $(touch /tmp/x) `id` | cat > /dev/null && exit 'a b' \u{e9}\u{1f600}";
    let cases: Vec<(Value, Vec<String>)> = vec![
        (
            json!({"project_id":p,"kind":"shell"}),
            ["shell", "create", "--project", &p, "--json"]
                .map(String::from)
                .to_vec(),
        ),
        (
            json!({"worktree_id":w,"kind":"shell"}),
            ["shell", "create", "--worktree", &w, "--json"]
                .map(String::from)
                .to_vec(),
        ),
        (
            json!({"worktree_id":w,"kind":"shell","unrestricted":false}),
            ["shell", "create", "--worktree", &w, "--json"]
                .map(String::from)
                .to_vec(),
        ),
        (
            json!({"worktree_id":w,"kind":"shell","command":hostile}),
            vec![
                "shell".into(),
                "create".into(),
                "--worktree".into(),
                w.clone(),
                "--command".into(),
                hostile.into(),
                "--json".into(),
            ],
        ),
        (
            json!({"project_id":p,"kind":"shell","command":" -x with a leading space"}),
            vec![
                "shell".into(),
                "create".into(),
                "--project".into(),
                p.clone(),
                "--command".into(),
                " -x with a leading space".into(),
                "--json".into(),
            ],
        ),
        (
            json!({"project_id":p,"kind":"shell","command":"x".repeat(4096)}),
            vec![
                "shell".into(),
                "create".into(),
                "--project".into(),
                p.clone(),
                "--command".into(),
                "x".repeat(4096),
                "--json".into(),
            ],
        ),
    ];
    for (params, argv) in cases {
        let before = f.calls().len();
        // The CLI answers with a session that matches the target asked for.
        f.cli_says(json!({
            "id": shell, "project_id": p, "worktree_id": w, "kind": "project",
            "cwd": "/x", "harness": null, "alive": true, "created_at_unix": 1
        }));
        let response = f.create(params.clone()).await;
        assert_eq!(response["ok"], true, "{params}: {response}");
        let calls = f.calls();
        // The target is looked up by its exact id, then the terminal is made.
        assert_eq!(calls.len(), before + 2, "{params}: look-up and creation");
        assert_eq!(calls.last().unwrap(), &argv, "{params}");
        let target = if params.get("project_id").is_some() {
            ("project", &p)
        } else {
            ("worktree", &w)
        };
        assert_eq!(
            calls[calls.len() - 2],
            vec![
                target.0.to_owned(),
                "show".into(),
                target.1.clone(),
                "--json".into()
            ],
            "{params}"
        );
    }
    for (kind, unrestricted, flags) in [
        ("codex", None, vec!["--harness", "codex"]),
        ("claude", None, vec!["--harness", "claude"]),
        ("grok", None, vec!["--harness", "grok"]),
        ("codex", Some(false), vec!["--harness", "codex"]),
        (
            "codex",
            Some(true),
            vec!["--harness", "codex", "--unrestricted"],
        ),
        (
            "claude",
            Some(true),
            vec!["--harness", "claude", "--unrestricted"],
        ),
        (
            "grok",
            Some(true),
            vec!["--harness", "grok", "--unrestricted"],
        ),
    ] {
        f.cli_says(json!({
            "id": shell, "project_id": p, "worktree_id": w, "kind": "project",
            "cwd": "/x", "harness": kind, "alive": true, "created_at_unix": 1
        }));
        let mut params = json!({"worktree_id":w,"kind":kind});
        if let Some(flag) = unrestricted {
            params["unrestricted"] = json!(flag);
        }
        let response = f.create(params.clone()).await;
        assert_eq!(response["ok"], true, "{params}: {response}");
        let mut expected: Vec<String> = ["shell", "create", "--worktree", &w]
            .map(String::from)
            .to_vec();
        expected.extend(flags.iter().map(|flag| flag.to_string()));
        expected.push("--json".into());
        assert_eq!(f.calls().last().unwrap(), &expected, "{params}");
    }
    // Only the look-up and the creation ran, and the question an agent left to the
    // desktop asks first: no listing, no second call.
    assert!(f.calls().iter().all(|c| c[1] == "show"
        || c[..2] == ["shell", "create"]
        || c[..2] == ["capabilities", "--json"]));
}

#[tokio::test]
async fn an_agent_left_to_the_desktop_follows_its_settings_when_the_cli_can_ask_them() {
    let f = Fixture::new();
    let (p, shell) = (f.project.clone(), new_uuid());
    f.set(
        "capabilities.json",
        &json!({"v": 1, "shell_create_as_settings": true}).to_string(),
    );
    for (kind, extra, flags, asks) in [
        // Left to the desktop: the CLI decides from Settings.
        (
            "codex",
            json!({"as_settings": true}),
            vec!["--harness", "codex", "--as-settings"],
            true,
        ),
        (
            "claude",
            json!({"as_settings": true}),
            vec!["--harness", "claude", "--as-settings"],
            true,
        ),
        (
            "grok",
            json!({"as_settings": true}),
            vec!["--harness", "grok", "--as-settings"],
            true,
        ),
        // Said: honored as it always was, without a question.
        (
            "codex",
            json!({"unrestricted": false}),
            vec!["--harness", "codex"],
            false,
        ),
        (
            "claude",
            json!({"unrestricted": true}),
            vec!["--harness", "claude", "--unrestricted"],
            false,
        ),
        // Left out, as an older phone does for its switch's off: restricted, as before.
        ("codex", json!({}), vec!["--harness", "codex"], false),
        // A plain shell is never unrestricted, and nothing is asked for it.
        ("shell", json!({}), vec![], false),
        ("shell", json!({"unrestricted": false}), vec![], false),
    ] {
        let harness = if kind == "shell" {
            Value::Null
        } else {
            json!(kind)
        };
        f.cli_says(json!({
            "id": shell, "project_id": p, "worktree_id": null, "kind": "project",
            "cwd": "/x", "harness": harness, "alive": true, "created_at_unix": 1
        }));
        let before = f.calls_of("capabilities", "--json").len();
        let mut params = json!({"project_id":p,"kind":kind});
        for (name, value) in extra.as_object().unwrap() {
            params[name] = value.clone();
        }
        let response = f.create(params.clone()).await;
        assert_eq!(response["ok"], true, "{params}: {response}");
        let mut expected: Vec<String> = ["shell", "create", "--project", &p]
            .map(String::from)
            .to_vec();
        expected.extend(flags.iter().map(|flag| flag.to_string()));
        expected.push("--json".into());
        assert_eq!(f.calls().last().unwrap(), &expected, "{params}");
        // A yes is remembered: the question is asked once at most.
        let asked = f.calls_of("capabilities", "--json").len() - before;
        assert!(asked <= usize::from(asks), "{params}: asked {asked} times");
    }
    assert_eq!(f.calls_of("capabilities", "--json").len(), 1);

    // A CLI that does not say so starts the agent restricted, as before.
    let older = Fixture::new();
    older.cli_says(json!({
        "id": shell, "project_id": older.project, "worktree_id": null, "kind": "project",
        "cwd": "/x", "harness": "codex", "alive": true, "created_at_unix": 1
    }));
    let response = older
        .create(json!({"project_id":older.project,"kind":"codex","as_settings":true}))
        .await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        older.calls().last().unwrap(),
        &[
            "shell",
            "create",
            "--project",
            &older.project,
            "--harness",
            "codex",
            "--json"
        ]
        .map(String::from)
        .to_vec()
    );
}

#[tokio::test]
async fn the_result_is_the_new_shell_and_its_list_entry_without_private_fields() {
    let f = Fixture::new();
    let shell = new_uuid();
    f.cli_says(f.session(&shell, json!("codex")));
    let response = f
        .create(json!({"worktree_id":f.worktree,"kind":"codex"}))
        .await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["result"],
        json!({
            "shell_id": shell,
            "shell": {
                "id": shell,
                "project_id": f.project,
                "worktree_id": f.worktree,
                "kind": "project",
                "cwd": "/Users/me/code/app",
                "harness": "codex",
                "alive": true,
                "created_at_unix": 1790000000u64
            }
        })
    );
    // The same projection `shells.list` uses: nothing about accounts, homes
    // or commands reaches the phone.
    let text = response.to_string();
    for private in [
        "me@example.com",
        ".codex",
        "acct",
        "unrestricted",
        "editor_path",
    ] {
        assert!(!text.contains(private), "{private} leaked: {text}");
    }
    // A project-only target is satisfied by the project's id alone: the shell
    // goes to the primary worktree, or none.
    let shell = new_uuid();
    let mut session = f.session(&shell, Value::Null);
    session["worktree_id"] = Value::Null;
    f.cli_says(session);
    let response = f
        .create(json!({"project_id":f.project,"kind":"shell"}))
        .await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["shell"]["worktree_id"], Value::Null);
    assert_eq!(response["result"]["shell"]["harness"], Value::Null);
}

#[tokio::test]
async fn a_session_that_is_not_the_one_asked_for_is_refused() {
    let f = Fixture::new();
    let good = new_uuid();
    let other = new_uuid();
    let params = json!({"worktree_id":f.worktree,"kind":"claude"});
    let mutate = |change: &dyn Fn(&mut Value)| {
        let mut session = f.session(&good, json!("claude"));
        change(&mut session);
        session
    };
    let bad = [
        mutate(&|s| s["id"] = json!("not-a-uuid")),
        mutate(&|s| s["id"] = json!(good.to_uppercase())),
        mutate(&|s| s["id"] = Value::Null),
        mutate(&|s| {
            s.as_object_mut().unwrap().remove("id");
        }),
        mutate(&|s| s["worktree_id"] = json!(other)),
        mutate(&|s| s["worktree_id"] = Value::Null),
        mutate(&|s| s["kind"] = json!("orchestrator")),
        mutate(&|s| s["harness"] = json!("codex")),
        mutate(&|s| s["harness"] = Value::Null),
        mutate(&|s| {
            s.as_object_mut().unwrap().remove("alive");
        }),
        mutate(&|s| s["alive"] = json!("yes")),
        mutate(&|s| {
            s.as_object_mut().unwrap().remove("cwd");
        }),
        mutate(&|s| {
            s.as_object_mut().unwrap().remove("created_at_unix");
        }),
        json!([f.session(&good, json!("claude"))]),
        json!("created"),
        Value::Null,
    ];
    for session in bad {
        f.cli_says(session.clone());
        let response = f.create(params.clone()).await;
        assert_eq!(code(&response), "cli_error", "{session}: {response}");
    }
    // A project target is checked against the project.
    let mut session = f.session(&good, Value::Null);
    session["project_id"] = json!(other);
    f.cli_says(session);
    let response = f
        .create(json!({"project_id":f.project,"kind":"shell"}))
        .await;
    assert_eq!(code(&response), "cli_error", "{response}");
    // Not JSON at all.
    f.set("create.json", "created shell 1234");
    let response = f.create(params).await;
    assert_eq!(code(&response), "cli_error", "{response}");
}

#[tokio::test]
async fn the_target_must_exist_under_exactly_the_id_given() {
    let f = Fixture::new();
    let (p, w) = (f.project.clone(), f.worktree.clone());
    let other = new_uuid();
    let creations = |f: &Fixture| f.calls_of("shell", "create").len();
    for (params, kind) in [
        (json!({"project_id":p,"kind":"shell"}), "project"),
        (json!({"worktree_id":w,"kind":"codex"}), "worktree"),
    ] {
        // The desktop does not know the id.
        f.set("show.error", &format!("No {kind} matches '{p}'"));
        let response = f.create(params.clone()).await;
        assert_eq!(code(&response), "not_found", "{kind}: {response}");
        assert_eq!(creations(&f), 0);
        // It knows something else by that name: a branch, a project name or a
        // path that spells the id. That is not the id.
        std::fs::remove_file(f.stub.path().join("show.error")).unwrap();
        f.set("show.id", &other);
        let response = f.create(params.clone()).await;
        assert_eq!(code(&response), "not_found", "{kind}: {response}");
        assert_eq!(
            message(&response),
            format!("{kind} not found on the desktop")
        );
        assert_eq!(creations(&f), 0);
        // Any other failure of the look-up is the CLI's, and still starts nothing.
        std::fs::remove_file(f.stub.path().join("show.id")).unwrap();
        f.set(
            "show.error",
            "More than one project matches 'x'; use its UUID",
        );
        let response = f.create(params).await;
        assert_eq!(code(&response), "cli_error", "{kind}: {response}");
        assert_eq!(creations(&f), 0);
        std::fs::remove_file(f.stub.path().join("show.error")).unwrap();
    }
}

#[tokio::test]
async fn a_session_that_was_not_asked_for_is_ended_rather_than_left_running() {
    let f = Fixture::new();
    let stray = new_uuid();
    // Made, but in some other worktree.
    let mut session = f.session(&stray, Value::Null);
    session["worktree_id"] = json!(new_uuid());
    f.cli_says(session);
    let response = f
        .create(json!({"worktree_id":f.worktree,"kind":"shell"}))
        .await;
    assert_eq!(code(&response), "cli_error", "{response}");
    for _ in 0..100 {
        if !f.calls_of("shell", "close").is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        f.calls_of("shell", "close"),
        vec![vec!["shell".to_owned(), "close".into(), stray]]
    );
    // Something that is not a session of ours is not closed.
    let before = f.calls_of("shell", "close").len();
    let mut orchestrator = f.session(&new_uuid(), Value::Null);
    orchestrator["kind"] = json!("orchestrator");
    f.cli_says(orchestrator);
    let response = f
        .create(json!({"worktree_id":f.worktree,"kind":"shell"}))
        .await;
    assert_eq!(code(&response), "cli_error", "{response}");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(f.calls_of("shell", "close").len(), before);
}

#[tokio::test]
async fn a_terminal_being_made_is_finished_when_the_request_is_dropped() {
    // The connector drops a request's task when its connection ends (relay
    // error, revocation): the CLI must not be killed between starting tmux and
    // writing the session down.
    let f = std::sync::Arc::new(Fixture::new());
    let shell = new_uuid();
    f.cli_says(f.session(&shell, Value::Null));
    f.set("create.hold", "");
    let task = {
        let f = f.clone();
        tokio::spawn(async move {
            f.create(json!({"worktree_id":f.worktree,"kind":"shell"}))
                .await
        })
    };
    // Wait until the CLI is running, then drop the request.
    for _ in 0..500 {
        if !f.calls_of("shell", "create").is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(f.calls_of("shell", "create").len(), 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    // Only now may the CLI finish: the request is gone, the CLI must not be.
    f.set("create.gate", "");
    for _ in 0..200 {
        if f.stub.path().join("create.ran").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        f.stub.path().join("create.ran").exists(),
        "the CLI was killed before it finished"
    );
}

#[tokio::test]
async fn the_cli_failures_the_phone_acts_on_have_their_own_codes() {
    let f = Fixture::new();
    let params = json!({"worktree_id":f.worktree,"kind":"codex"});
    let cases = [
        (
            "No project matches 'aaaaaaaa-0000-4000-8000-000000000000'",
            "not_found",
            "project not found on the desktop",
        ),
        (
            "No worktree matches 'aaaaaaaa-0000-4000-8000-000000000000'",
            "not_found",
            "worktree not found on the desktop",
        ),
        (
            "resolve /Users/me/gone: No such file or directory (os error 2)",
            "not_found",
            "the folder of this project or worktree no longer exists on the desktop",
        ),
        (
            "codex is not installed or is not on PATH",
            "harness_unavailable",
            "codex is not installed or is not on PATH",
        ),
        (
            "claude is not installed or is not on PATH",
            "harness_unavailable",
            "claude is not installed or is not on PATH",
        ),
        (
            "grok is not installed or is not on PATH",
            "harness_unavailable",
            "grok is not installed or is not on PATH",
        ),
        (
            "Cua Driver is not installed. Open RiWork Settings, or run `riwork setup`.",
            "harness_unavailable",
            "Cua Driver is not installed. Open RiWork Settings, or run `riwork setup`.",
        ),
        // Anything else is the CLI's own words, without the wrapper.
        (
            "tmux: no server running",
            "cli_error",
            "tmux: no server running",
        ),
        (
            "More than one worktree matches 'main'; use its UUID",
            "cli_error",
            "More than one worktree matches 'main'; use its UUID",
        ),
        // Not the folder: the CLI could not find itself.
        (
            "resolve RiWork executable: No such file or directory (os error 2)",
            "cli_error",
            "resolve RiWork executable: No such file or directory (os error 2)",
        ),
        // A look-alike is not the installation error.
        (
            "node is not installed or is not on PATH",
            "cli_error",
            "node is not installed or is not on PATH",
        ),
        (
            "something about codex is not installed or is not on PATH",
            "cli_error",
            "something about codex is not installed or is not on PATH",
        ),
    ];
    for (line, expected, text) in cases {
        f.cli_fails(line);
        let response = f.create(params.clone()).await;
        assert_eq!(code(&response), expected, "{line}: {response}");
        assert_eq!(message(&response), text, "{line}");
    }
    // Only the first line of the CLI's error decides, so a later one that
    // looks like a known error is not mistaken for it.
    f.cli_fails("tmux: boom\nNo project matches 'x'");
    let response = f.create(params).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    assert_eq!(message(&response), "tmux: boom");
}

#[tokio::test]
async fn a_revoked_device_creates_and_closes_nothing() {
    let f = Fixture::new();
    f.cli_says(f.session(&new_uuid(), Value::Null));
    f.rpc.storage.revoke(&f.device).unwrap();
    assert!(
        f.rpc
            .handle(
                &f.device,
                req(
                    "shell.create",
                    json!({"project_id":f.project,"kind":"shell"})
                )
            )
            .await
            .is_err()
    );
    assert!(
        f.rpc
            .handle(
                &f.device,
                req("shell.close", json!({"shell_id":new_uuid()}))
            )
            .await
            .is_err()
    );
    assert!(f.calls().is_empty(), "{:?}", f.calls());
}

#[tokio::test]
async fn a_project_terminal_is_closed_by_its_full_id_only() {
    let f = Fixture::new();
    let shell = new_uuid();
    f.set(
        "shells.json",
        &json!([{"id":shell,"kind":"project","alive":true}]).to_string(),
    );
    let response = f.close(&shell).await;
    assert_eq!(
        response["result"],
        json!({"shell_id":shell,"status":"closed"}),
        "{response}"
    );
    assert_eq!(
        f.calls_of("shell", "close"),
        vec![vec!["shell".to_owned(), "close".into(), shell.clone()]]
    );
    // A terminal that already exited is still a terminal: closing it takes it
    // off the desktop's list.
    let gone = new_uuid();
    f.set(
        "shells.json",
        &json!([{"id":gone,"kind":"project","alive":false}]).to_string(),
    );
    let response = f.close(&gone).await;
    assert_eq!(response["ok"], true, "{response}");
    // An id the desktop does not list is not passed on.
    let before = f.calls_of("shell", "close").len();
    let response = f.close(&new_uuid()).await;
    assert_eq!(code(&response), "not_found", "{response}");
    assert_eq!(f.calls_of("shell", "close").len(), before);
}

#[tokio::test]
async fn an_orchestrator_is_not_closed_from_the_phone() {
    let f = Fixture::new();
    let orchestrator = new_uuid();
    f.set(
        "orchestrators.json",
        &json!([{"id":orchestrator,"kind":"orchestrator","alive":true}]).to_string(),
    );
    let response = f.close(&orchestrator).await;
    assert_eq!(code(&response), "invalid_request", "{response}");
    assert!(f.calls_of("shell", "close").is_empty());
}

#[tokio::test]
async fn close_failures_are_not_found_or_cli_errors() {
    let f = Fixture::new();
    let shell = new_uuid();
    f.set(
        "shells.json",
        &json!([{"id":shell,"kind":"project","alive":true}]).to_string(),
    );
    // Closed by someone else between the lookup and the call.
    f.set("close.error", &format!("unknown shell {shell}"));
    let response = f.close(&shell).await;
    assert_eq!(code(&response), "not_found", "{response}");
    // Anything else, such as a tmux that cannot answer, is the CLI's.
    f.set("close.error", "tmux: timed out");
    let response = f.close(&shell).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    assert!(message(&response).contains("tmux: timed out"), "{response}");
}

#[tokio::test]
async fn closing_a_shell_this_connection_resized_releases_the_resize_first() {
    let f = Fixture::new();
    let shell = new_uuid();
    f.set(
        "shells.json",
        &json!([{"id":shell,"kind":"project","alive":true}]).to_string(),
    );
    let mut viewport = Viewport::new(f.rpc.cli.clone(), f.device.clone());
    let resized = f
        .rpc
        .handle_in(
            &f.device,
            req(
                "shell.resize",
                json!({"shell_id":shell,"columns":43,"rows":17}),
            ),
            Some(&mut viewport),
        )
        .await
        .unwrap();
    assert_eq!(resized["ok"], true, "{resized}");
    let closed = f
        .rpc
        .handle_in(
            &f.device,
            req("shell.close", json!({"shell_id":shell})),
            Some(&mut viewport),
        )
        .await
        .unwrap();
    assert_eq!(closed["ok"], true, "{closed}");
    let words: Vec<String> = f.calls().iter().map(|c| c[1].clone()).collect();
    let clear = words.iter().position(|w| w == "resize-clear").unwrap();
    let close = words.iter().position(|w| w == "close").unwrap();
    assert!(clear < close, "{words:?}");
    // Nothing is left to release when the connection ends.
    drop(viewport);
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(
        f.calls_of("shell", "resize-clear").len(),
        1,
        "{:?}",
        f.calls()
    );
}

#[tokio::test]
async fn closing_another_shell_leaves_this_connections_resize_alone() {
    let f = Fixture::new();
    let (pinned, other) = (new_uuid(), new_uuid());
    f.set(
        "shells.json",
        &json!([{"id":pinned,"kind":"project","alive":true},
                {"id":other,"kind":"project","alive":true}])
        .to_string(),
    );
    let mut viewport = Viewport::new(f.rpc.cli.clone(), f.device.clone());
    f.rpc
        .handle_in(
            &f.device,
            req(
                "shell.resize",
                json!({"shell_id":pinned,"columns":43,"rows":17}),
            ),
            Some(&mut viewport),
        )
        .await
        .unwrap();
    let closed = f
        .rpc
        .handle_in(
            &f.device,
            req("shell.close", json!({"shell_id":other})),
            Some(&mut viewport),
        )
        .await
        .unwrap();
    assert_eq!(closed["ok"], true, "{closed}");
    assert!(f.calls_of("shell", "resize-clear").is_empty());
}

/// The real CLI in a throwaway RIWORK_HOME (and its own tmux server), behind a
/// wrapper that also puts a stand-in `claude` and Cua driver first on PATH.
struct RealCli {
    dir: tempfile::TempDir,
    wrapper: PathBuf,
}
impl RealCli {
    fn new(cli: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for (name, body) in [("claude", "exec sleep 300"), ("cua-driver", "exit 0")] {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let wrapper = dir.path().join("riwork");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexport RIWORK_HOME='{home}' RIWORK_RUNTIME_DIR='{home}/runtime' \
                 RIWORK_CUA_DRIVER='{bin}/cua-driver' PATH='{bin}':\"$PATH\"\nexec '{cli}' \"$@\"\n",
                home = home.display(),
                bin = bin.display(),
                cli = cli.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, wrapper }
    }
    fn run(&self, args: &[&str]) -> Value {
        let output = std::process::Command::new(&self.wrapper)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
impl Drop for RealCli {
    fn drop(&mut self) {
        // Only what this test made: the home is its own.
        let listed = std::process::Command::new(&self.wrapper)
            .args(["shell", "list", "--all", "--json"])
            .output();
        let ids: Vec<String> = listed
            .ok()
            .and_then(|out| serde_json::from_slice::<Value>(&out.stdout).ok())
            .and_then(|list| {
                list.as_array().map(|list| {
                    list.iter()
                        .filter_map(|s| s["id"].as_str().map(str::to_owned))
                        .collect()
                })
            })
            .unwrap_or_default();
        for id in ids {
            let _ = std::process::Command::new(&self.wrapper)
                .args(["shell", "close", &id])
                .output();
        }
        let _ = &self.dir;
    }
}

#[tokio::test]
#[ignore = "requires RIWORK_TEST_CLI; isolated real tmux and CLI, creates and closes only its own terminals"]
async fn the_real_cli_creates_lists_and_closes_a_terminal_through_the_rpc() {
    let cli = PathBuf::from(std::env::var_os("RIWORK_TEST_CLI").expect("set RIWORK_TEST_CLI"));
    assert!(cli.is_absolute(), "absolute CLI path required");
    let real = RealCli::new(&cli);
    let repo = real.dir.path().join("app");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "--initial-branch=main", "--template="],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "x",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(&args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let project = real.run(&["project", "add", repo.to_str().unwrap(), "--json"])["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let storage_dir = tempfile::tempdir().unwrap();
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
    let rpc = Rpc::new(real.wrapper.clone(), storage);
    let call = |method: &str, params: Value| {
        let (rpc, device) = (&rpc, device.clone());
        let request = req(method, params);
        async move { rpc.handle(&device, request).await.unwrap() }
    };

    let worktrees = call("worktrees.list", json!({"project_id":project})).await;
    let worktree = worktrees["result"]["worktrees"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // A plain shell in a worktree, then an agent in the project.
    let shell = call(
        "shell.create",
        json!({"worktree_id":worktree,"kind":"shell","command":"exec sleep 300"}),
    )
    .await;
    assert_eq!(shell["ok"], true, "{shell}");
    let shell_id = shell["result"]["shell_id"].as_str().unwrap().to_owned();
    assert_eq!(shell["result"]["shell"]["worktree_id"], worktree.as_str());
    assert_eq!(shell["result"]["shell"]["harness"], Value::Null);
    assert_eq!(shell["result"]["shell"]["alive"], true);
    let agent = call(
        "shell.create",
        json!({"project_id":project,"kind":"claude"}),
    )
    .await;
    assert_eq!(agent["ok"], true, "{agent}");
    let agent_id = agent["result"]["shell_id"].as_str().unwrap().to_owned();
    assert_eq!(agent["result"]["shell"]["harness"], "claude");

    // `shells.list` shows both, each as `shell.create` returned it.
    let listed = call("shells.list", json!({"project_id":project})).await;
    let listed = listed["result"]["shells"].as_array().unwrap().clone();
    assert!(listed.contains(&shell["result"]["shell"]), "{listed:?}");
    assert!(listed.contains(&agent["result"]["shell"]), "{listed:?}");
    // And the new shell can be read at once.
    let output = call("shell.output", json!({"shell_id":shell_id,"lines":5})).await;
    assert_eq!(output["ok"], true, "{output}");

    // Things that do not exist are not_found, and start nothing.
    let missing = call(
        "shell.create",
        json!({"worktree_id":new_uuid(),"kind":"shell"}),
    )
    .await;
    assert_eq!(code(&missing), "not_found", "{missing}");
    let missing = call(
        "shell.create",
        json!({"project_id":new_uuid(),"kind":"codex"}),
    )
    .await;
    assert_eq!(code(&missing), "not_found", "{missing}");
    let after = call("shells.list", json!({"project_id":project})).await;
    assert_eq!(after["result"]["shells"].as_array().unwrap().len(), 2);

    // Closing ends it and takes it off the list; a second close is not_found.
    for id in [&shell_id, &agent_id] {
        let closed = call("shell.close", json!({"shell_id":id})).await;
        assert_eq!(
            closed["result"],
            json!({"shell_id":id,"status":"closed"}),
            "{closed}"
        );
    }
    let again = call("shell.close", json!({"shell_id":shell_id})).await;
    assert_eq!(code(&again), "not_found", "{again}");
    let after = call("shells.list", json!({"project_id":project})).await;
    assert_eq!(after["result"]["shells"], json!([]));
}
