//! The activity and recency fields of `projects.list`, `shells.list` and
//! `orchestrators.list` (docs/remote-protocol.md, "Activity and recency
//! extension"), including `last_activity_unix` on projects and shells: which
//! fields the phone is shown, that an older CLI which has
//! none of them still answers exactly as before, and that a field in the wrong
//! shape is left out instead of passed on. Against a stub CLI that prints the
//! JSON in `projects.json`, `shells.json` and `orchestrators.json`.
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
        }
    }
    fn says(&self, file: &str, value: Value) {
        std::fs::write(self.stub.path().join(file), value.to_string()).unwrap();
    }
    async fn call(&self, method: &str, params: Value) -> Value {
        let response = self
            .rpc
            .handle(&self.device, req(method, params))
            .await
            .unwrap();
        assert_eq!(response["ok"], true, "{response}");
        response["result"].clone()
    }
}

/// Prints the file for the list that was asked for, `[]` if it is missing.
fn stub_cli(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let cli = dir.join("fake-riwork");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\n\
             d='{dir}'\n\
             case \"$1 $2\" in\n\
             'project list') f=projects.json;;\n\
             'shell list') f=shells.json;;\n\
             'orchestrator list') f=orchestrators.json;;\n\
             esac\n\
             if [ -e \"$d/$f\" ]; then cat \"$d/$f\"; else echo '[]'; fi\n",
            dir = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}

fn project(id: &str) -> Value {
    json!({
        "id": id,
        "name": "App",
        "root": "/Users/me/code/app",
        "created_at": 1790000000u64,
        // What the phone must never see.
        "repository_roots": ["/Users/me/code/app"],
        "folder_id": "folder-secret",
        "notify_on_agent_done": true,
        "codex_account": {"mode": "inherit", "account_id": "acct-secret"}
    })
}

fn session(id: &str, project: &str) -> Value {
    json!({
        "id": id,
        "project_id": project,
        "worktree_id": null,
        "kind": "project",
        "cwd": "/Users/me/code/app",
        "command": "claude --secret-flag",
        "editor_path": null,
        "harness": "claude",
        "unrestricted": true,
        "codex_account_id": "acct-secret",
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

const PRIVATE: [&str; 6] = [
    "repository_roots",
    "folder-secret",
    "acct-secret",
    "secret-flag",
    "me@example.com",
    "/Users/me/.codex",
];

#[tokio::test]
async fn projects_list_passes_on_recency_and_agent_counts() {
    let f = Fixture::new();
    let id = new_uuid();
    let mut listed = project(&id);
    listed["last_edited_unix"] = json!(1790000500u64);
    listed["last_activity_unix"] = json!(1790000900u64);
    listed["agents"] = json!({"working": 2, "waiting": 1, "done": 3});
    f.says("projects.json", json!([listed]));
    let result = f.call("projects.list", json!({})).await;
    assert_eq!(
        result["projects"],
        json!([{
            "id": id,
            "name": "App",
            "root": "/Users/me/code/app",
            "created_at": 1790000000u64,
            "last_edited_unix": 1790000500u64,
            "last_activity_unix": 1790000900u64,
            "agents": {"working": 2, "waiting": 1, "done": 3}
        }])
    );
    let text = result.to_string();
    for private in PRIVATE {
        assert!(!text.contains(private), "{private} leaked: {text}");
    }
    // The two counts of the frozen interface alone are enough.
    let mut minimal = project(&id);
    minimal["agents"] = json!({"working": 0, "waiting": 0});
    f.says("projects.json", json!([minimal]));
    let result = f.call("projects.list", json!({})).await;
    assert_eq!(
        result["projects"][0]["agents"],
        json!({"working": 0, "waiting": 0})
    );
}

#[tokio::test]
async fn an_older_cli_without_the_new_fields_answers_exactly_as_before() {
    let f = Fixture::new();
    let (id, shell) = (new_uuid(), new_uuid());
    f.says("projects.json", json!([project(&id)]));
    f.says("shells.json", json!([session(&shell, &id)]));
    f.says("orchestrators.json", json!([session(&shell, &id)]));
    let projects = f.call("projects.list", json!({})).await;
    assert_eq!(
        projects["projects"],
        json!([{"id": id, "name": "App", "root": "/Users/me/code/app", "created_at": 1790000000u64}])
    );
    let expected = json!([{
        "id": shell, "project_id": id, "worktree_id": null, "kind": "project",
        "cwd": "/Users/me/code/app", "harness": "claude", "alive": true,
        "created_at_unix": 1790000000u64
    }]);
    let shells = f.call("shells.list", json!({"project_id": id})).await;
    assert_eq!(shells["shells"], expected);
    let orchestrators = f.call("orchestrators.list", json!({})).await;
    assert_eq!(orchestrators["orchestrators"], expected);
}

#[tokio::test]
async fn shells_and_orchestrators_pass_on_activity_and_subagents() {
    let f = Fixture::new();
    let (id, shell) = (new_uuid(), new_uuid());
    let mut working = session(&shell, &id);
    working["last_activity_unix"] = json!(1790000800u64);
    working["activity"] = json!("working");
    working["activity_since_unix"] = json!(1790000100u64);
    working["subagents_working"] = json!(2);
    working["subagent_kinds"] = json!(["general-purpose", "Explore"]);
    let mut exited = session(&new_uuid(), &id);
    exited["activity"] = json!("exited");
    exited["alive"] = json!(false);
    f.says("shells.json", json!([working, exited]));
    f.says("orchestrators.json", json!([working]));
    let shells = f.call("shells.list", json!({"project_id": id})).await;
    assert_eq!(
        shells["shells"][0],
        json!({
            "id": shell, "project_id": id, "worktree_id": null, "kind": "project",
            "cwd": "/Users/me/code/app", "harness": "claude", "alive": true,
            "created_at_unix": 1790000000u64,
            "last_activity_unix": 1790000800u64,
            "activity": "working", "activity_since_unix": 1790000100u64,
            "subagents_working": 2, "subagent_kinds": ["general-purpose", "Explore"]
        })
    );
    assert_eq!(shells["shells"][1]["activity"], "exited");
    assert!(shells["shells"][1].get("subagents_working").is_none());
    assert!(shells["shells"][1].get("last_activity_unix").is_none());
    let orchestrators = f.call("orchestrators.list", json!({})).await;
    assert_eq!(orchestrators["orchestrators"][0]["activity"], "working");
    assert_eq!(orchestrators["orchestrators"][0]["subagents_working"], 2);
    assert_eq!(
        orchestrators["orchestrators"][0]["last_activity_unix"],
        1790000800u64
    );
    for private in PRIVATE {
        assert!(
            !shells.to_string().contains(private) && !orchestrators.to_string().contains(private),
            "{private} leaked"
        );
    }
    // Every word of the contract passes.
    for word in ["working", "waiting", "done", "unknown", "exited"] {
        let mut one = session(&shell, &id);
        one["activity"] = json!(word);
        f.says("shells.json", json!([one]));
        let shells = f.call("shells.list", json!({"project_id": id})).await;
        assert_eq!(shells["shells"][0]["activity"], word);
    }
}

#[tokio::test]
async fn a_field_in_the_wrong_shape_is_left_out_and_the_rest_stays() {
    let f = Fixture::new();
    let (id, shell) = (new_uuid(), new_uuid());
    let wrong_sessions = [
        ("last_activity_unix", json!("1790000800")),
        ("last_activity_unix", json!(-1)),
        ("last_activity_unix", json!(1.5)),
        ("last_activity_unix", json!(null)),
        ("last_activity_unix", json!([1790000800u64])),
        ("activity", json!("busy")),
        ("activity", json!("Working")),
        ("activity", json!(true)),
        ("activity", json!(null)),
        ("activity_since_unix", json!("1790000100")),
        ("activity_since_unix", json!(-1)),
        ("activity_since_unix", json!(1.5)),
        ("subagents_working", json!(-2)),
        ("subagents_working", json!("2")),
        ("subagents_working", json!(null)),
        ("subagent_kinds", json!("general-purpose")),
        ("subagent_kinds", json!([7])),
        ("subagent_kinds", json!(["has space"])),
        ("subagent_kinds", json!(["/etc/passwd"])),
        ("subagent_kinds", json!([""])),
        ("subagent_kinds", json!(["x".repeat(41)])),
        (
            "subagent_kinds",
            json!(["a", "b", "c", "d", "e", "f", "g", "h", "i"]),
        ),
    ];
    for (field, value) in wrong_sessions {
        let mut one = session(&shell, &id);
        one[field] = value.clone();
        f.says("shells.json", json!([one]));
        let shells = f.call("shells.list", json!({"project_id": id})).await;
        let listed = &shells["shells"][0];
        assert!(listed.get(field).is_none(), "{field}={value} was passed on");
        assert_eq!(listed["id"], shell, "{field}={value}");
        assert_eq!(listed["alive"], true, "{field}={value}");
    }
    let wrong_projects = [
        ("last_edited_unix", json!("1790000500")),
        ("last_edited_unix", json!(-5)),
        ("last_edited_unix", json!(1.5)),
        ("last_edited_unix", json!(null)),
        ("last_activity_unix", json!("1790000900")),
        ("last_activity_unix", json!(-5)),
        ("last_activity_unix", json!(1.5)),
        ("last_activity_unix", json!(null)),
        ("last_activity_unix", json!({"at": 1790000900u64})),
        ("agents", json!(3)),
        ("agents", json!([1, 2])),
        ("agents", json!({})),
        ("agents", json!({"working": 1})),
        ("agents", json!({"working": 1, "waiting": "0"})),
        ("agents", json!({"working": -1, "waiting": 0})),
        ("agents", json!({"working": 1, "waiting": 0, "done": 1.5})),
        ("agents", json!({"working": 1, "waiting": 0, "path": 1})),
        (
            "agents",
            json!({"working": 1, "waiting": 0, "done": 0, "x": 0}),
        ),
    ];
    for (field, value) in wrong_projects {
        let mut one = project(&id);
        one[field] = value.clone();
        f.says("projects.json", json!([one]));
        let projects = f.call("projects.list", json!({})).await;
        let listed = &projects["projects"][0];
        assert!(listed.get(field).is_none(), "{field}={value} was passed on");
        assert_eq!(listed["name"], "App", "{field}={value}");
    }
}

#[tokio::test]
async fn one_wrong_field_does_not_take_a_correct_one_with_it() {
    let f = Fixture::new();
    let id = new_uuid();
    let mut one = project(&id);
    one["last_edited_unix"] = json!(1790000500u64);
    one["last_activity_unix"] = json!("recently");
    one["agents"] = json!({"working": "many", "waiting": 0});
    f.says("projects.json", json!([one]));
    let projects = f.call("projects.list", json!({})).await;
    assert_eq!(projects["projects"][0]["last_edited_unix"], 1790000500u64);
    assert!(projects["projects"][0].get("last_activity_unix").is_none());
    assert!(projects["projects"][0].get("agents").is_none());
    // And the other way round: a good activity time survives a bad edit time.
    let mut other = project(&id);
    other["last_edited_unix"] = json!(-1);
    other["last_activity_unix"] = json!(1790000900u64);
    f.says("projects.json", json!([other]));
    let projects = f.call("projects.list", json!({})).await;
    assert!(projects["projects"][0].get("last_edited_unix").is_none());
    assert_eq!(projects["projects"][0]["last_activity_unix"], 1790000900u64);
}

#[tokio::test]
async fn a_shell_list_larger_than_one_reply_is_trimmed_to_what_the_phone_sees() {
    // Launch commands (Claude's settings, hooks and prompts) made the CLI's own
    // list far larger than one encrypted reply, and every lookup failed.
    let f = Fixture::new();
    let project_id = new_uuid();
    let long_command = format!("claude --settings '{}'", "x".repeat(4096));
    let shells: Vec<Value> = (0..80)
        .map(|_| {
            let mut shell = session(&new_uuid(), &project_id);
            shell["command"] = json!(long_command);
            shell
        })
        .collect();
    let raw = Value::Array(shells).to_string();
    assert!(
        raw.len() > riwork_remote::MAX_PLAINTEXT * 2,
        "{}",
        raw.len()
    );
    f.says(
        "shells.json",
        Value::Array(serde_json::from_str::<Vec<Value>>(&raw).unwrap()),
    );
    let result = f
        .call("shells.list", json!({"project_id": project_id}))
        .await;
    let listed = result["shells"].as_array().unwrap();
    assert_eq!(listed.len(), 80);
    assert!(listed.iter().all(|shell| shell.get("command").is_none()));
    assert!(result.to_string().len() < riwork_remote::MAX_PLAINTEXT);
}
