//! The chat RPCs (`chats.list`, `chat.create`, `chat.events`, `chat.command`,
//! `chat.stop`): validation before any CLI runs, the argv they turn into (one
//! argument per value, never a shell string), what they accept back from the CLI,
//! how an events page is cut to fit a reply, and their errors, against a stub CLI
//! that records its argv and prints whatever the test put in its directory.
use riwork_remote::{MAX_PLAINTEXT, config::Storage, link, rpc::Rpc, viewport::Viewport};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
fn words(line: &[&str]) -> Vec<String> {
    line.iter().map(|word| (*word).to_owned()).collect()
}

struct Fixture {
    _storage_dir: tempfile::TempDir,
    stub: tempfile::TempDir,
    rpc: Rpc,
    device: String,
    project: String,
    worktree: String,
    chat: String,
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
        warm(&cli);
        Self {
            rpc: Rpc::new(cli, storage),
            _storage_dir: storage_dir,
            stub,
            device,
            project: new_uuid(),
            worktree: new_uuid(),
            chat: new_uuid(),
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
    async fn call(&self, method: &str, params: Value) -> Value {
        self.rpc
            .handle(&self.device, req(method, params))
            .await
            .unwrap()
    }
    /// A session whose replies may be deflated, as after `link.configure`.
    async fn call_deflating(&self, method: &str, params: Value) -> Value {
        let viewport = tokio::sync::Mutex::new(None::<Viewport>);
        self.rpc
            .handle_shared_up_to(
                &self.device,
                req(method, params),
                &viewport,
                link::MAX_INFLATED,
            )
            .await
            .unwrap()
    }
    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.stub.path().join(name), value).unwrap();
    }
    fn unset(&self, name: &str) {
        let _ = std::fs::remove_file(self.stub.path().join(name));
    }
    fn says(&self, name: &str, value: &Value) {
        self.set(name, &value.to_string());
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
    /// Chat calls, not counting the questions about what the CLI can do.
    fn chat_calls(&self) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| c.first().is_some_and(|w| w == "chat"))
            .collect()
    }
    /// A chat as the CLI prints it.
    fn info(&self, id: &str) -> Value {
        json!({
            "id": id,
            "provider": "codex",
            "project_id": self.project,
            "worktree_id": self.worktree,
            "cwd": "/Users/me/code/app",
            "title": "Codex chat",
            "created_at_unix": 1790000000u64,
            "provider_thread_id": "thread-1",
            "approval_mode": "supervised",
            "codex_account_id": "acct",
            "state": {"state": "idle"}
        })
    }
    /// An event as the CLI prints it in a page.
    fn event(seq: u64, text: &str) -> Value {
        json!({"seq": seq, "event": {
            "event": "item_completed",
            "item": {"id": format!("agent-{seq}"), "status": "completed",
                     "body": {"type": "agent_message", "text": text}}
        }})
    }
    fn page(&self, events: &[Value], next: u64, more: bool) -> Value {
        json!({"chat_id": self.chat, "events": events, "next": next, "more": more})
    }
}

/// Runs a script that was just written once. The first run of a new executable can take
/// seconds on a busy Mac (the system looks at it first), more than a test that bounds a
/// call should be left to depend on; the stub logs nothing for `warm`.
fn warm(cli: &Path) {
    let _ = std::process::Command::new(cli).arg("warm").output();
}

/// Logs each call (arguments separated by U+001F). `capabilities --json` prints
/// `capabilities.out` (default: `"chat":true`), or fails like a CLI from before
/// `capabilities` if `capabilities.unknown` exists. `project show` and
/// `worktree show` print the id they were asked for, or the one in `show.id`, or
/// fail with the line in `show.error`. `chat list` prints `list.json` (default
/// `[]`). `chat new` waits `create.delay` seconds if that exists, marks
/// `create.ran`, then prints `create.json`. `chat events` waits `events.delay`
/// and prints `events.json`. `chat command` and `chat stop` print the chat they
/// were given. `chat models` prints `models.json`. Each of the `chat` calls fails with the
/// line in its `.error` file (`list.error`, `create.error`, `events.error`, `command.error`,
/// `stop.error`, `models.error`) if that exists.
fn stub_cli(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let cli = dir.join("fake-riwork");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\n\
             d='{dir}'\n\
             [ \"$1\" = warm ] && exit 0\n\
             for a in \"$@\"; do printf '%s\\037' \"$a\"; done >> \"$d/argv.log\"\n\
             printf '\\n' >> \"$d/argv.log\"\n\
             fail() {{ if [ -e \"$d/$1.error\" ]; then printf 'riwork: %s\\n' \"$(cat \"$d/$1.error\")\" >&2; exit 2; fi; }}\n\
             case \"$1 $2\" in\n\
             'capabilities --json')\n\
               if [ -e \"$d/capabilities.unknown\" ]; then echo \"riwork: Unknown invocation 'capabilities'\" >&2; exit 2; fi\n\
               if [ -e \"$d/capabilities.out\" ]; then cat \"$d/capabilities.out\"; else printf '{{\"v\":1,\"chat\":true}}'; fi;;\n\
             'project show'|'worktree show')\n\
               fail show\n\
               if [ -e \"$d/show.id\" ]; then printf '{{\"id\":\"%s\"}}' \"$(cat \"$d/show.id\")\"; else printf '{{\"id\":\"%s\"}}' \"$3\"; fi;;\n\
             'chat list') fail list; if [ -e \"$d/list.json\" ]; then cat \"$d/list.json\"; else echo '[]'; fi;;\n\
             'chat new')\n\
               if [ -e \"$d/create.delay\" ]; then sleep \"$(cat \"$d/create.delay\")\"; fi\n\
               touch \"$d/create.ran\"\n\
               fail create\n\
               cat \"$d/create.json\";;\n\
             'chat snapshot') fail snapshot; cat \"$d/snapshot.json\";;\n\
             'chat events')\n\
               if [ -e \"$d/events.delay\" ]; then sleep \"$(cat \"$d/events.delay\")\"; fi\n\
               fail events\n\
               cat \"$d/events.json\";;\n\
             'chat command') fail command; printf '{{\"id\":\"%s\",\"status\":\"ok\"}}' \"$3\";;\n\
             'chat stop') fail stop; printf '{{\"id\":\"%s\",\"state\":\"stopped\"}}' \"$3\";;\n\
             'chat models') fail models; cat \"$d/models.json\";;\n\
             esac\n",
            dir = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}

/// Text that deflate cannot shrink much: printable noise.
fn noise(length: usize, mut x: u64) -> String {
    (0..length)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'!' + (x % 90) as u8)
        })
        .collect()
}

#[tokio::test]
async fn parameters_are_validated_before_any_cli_runs() {
    let f = Fixture::without_cli();
    let (p, w, c) = (f.project.clone(), f.worktree.clone(), f.chat.clone());
    let long = |n: usize| "x".repeat(n);
    let bad: Vec<(&str, Value)> = vec![
        // Not an object, or with a field that is not ours.
        ("chats.list", json!(null)),
        ("chats.list", json!([])),
        ("chats.list", json!({"all":true})),
        ("chats.list", json!({"project_id":null})),
        ("chats.list", json!({"project_id":7})),
        // Ids are full canonical UUIDs, never names, paths or prefixes.
        ("chats.list", json!({"project_id":"app"})),
        ("chats.list", json!({"project_id":&p[..8]})),
        ("chats.list", json!({"project_id":p.to_uppercase()})),
        ("chats.list", json!({"project_id":"--project"})),
        ("chat.create", json!({})),
        ("chat.create", json!({"provider":"codex"})),
        ("chat.create", json!({"project_id":p})),
        ("chat.create", json!({"provider":"gemini","project_id":p})),
        ("chat.create", json!({"provider":"Codex","project_id":p})),
        ("chat.create", json!({"provider":null,"project_id":p})),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"worktree_id":w}),
        ),
        ("chat.create", json!({"provider":"codex","project_id":null})),
        (
            "chat.create",
            json!({"provider":"codex","project_id":"app"}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","worktree_id":"main"}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"extra":1}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"approval_mode":"reckless"}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"approval_mode":"auto-edit"}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"approval_mode":null}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"approval_mode":1}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"model":long(101)}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"model":"a\nb"}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"model":7}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"effort":long(33)}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"effort":"hi\u{7}"}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"fast":"yes"}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"fast":1}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"fast":null}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"title":long(201)}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"title":"a\u{2028}b"}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"title":"tab\there"}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":p,"title":null}),
        ),
        // Events: everything required, in range, integers.
        ("chat.events", json!({})),
        ("chat.events", json!({"chat_id":c})),
        ("chat.events", json!({"chat_id":c,"since":0})),
        ("chat.events", json!({"chat_id":c,"wait_ms":0})),
        ("chat.events", json!({"since":0,"wait_ms":0})),
        (
            "chat.events",
            json!({"chat_id":"chat","since":0,"wait_ms":0}),
        ),
        (
            "chat.events",
            json!({"chat_id":&c[..8],"since":0,"wait_ms":0}),
        ),
        (
            "chat.events",
            json!({"chat_id":c.to_uppercase(),"since":0,"wait_ms":0}),
        ),
        ("chat.events", json!({"chat_id":c,"since":-1,"wait_ms":0})),
        ("chat.events", json!({"chat_id":c,"since":1.5,"wait_ms":0})),
        ("chat.events", json!({"chat_id":c,"since":"0","wait_ms":0})),
        ("chat.events", json!({"chat_id":c,"since":null,"wait_ms":0})),
        ("chat.events", json!({"chat_id":c,"since":0,"wait_ms":-1})),
        (
            "chat.events",
            json!({"chat_id":c,"since":0,"wait_ms":25001}),
        ),
        (
            "chat.events",
            json!({"chat_id":c,"since":0,"wait_ms":5000.0}),
        ),
        ("chat.events", json!({"chat_id":c,"since":0,"wait_ms":"5"})),
        (
            "chat.events",
            json!({"chat_id":c,"since":0,"wait_ms":0,"max_events":0}),
        ),
        (
            "chat.events",
            json!({"chat_id":c,"since":0,"wait_ms":0,"max_events":2001}),
        ),
        (
            "chat.events",
            json!({"chat_id":c,"since":0,"wait_ms":0,"max_events":null}),
        ),
        (
            "chat.events",
            json!({"chat_id":c,"since":0,"wait_ms":0,"max":5}),
        ),
        (
            "chat.events",
            json!({"chat_id":c,"since":0,"wait_ms":0,"max_bytes":5}),
        ),
        // Commands.
        ("chat.command", json!({})),
        ("chat.command", json!({"chat_id":c})),
        ("chat.command", json!({"command":{"command":"stop"}})),
        ("chat.command", json!({"chat_id":c,"command":null})),
        ("chat.command", json!({"chat_id":c,"command":"stop"})),
        ("chat.command", json!({"chat_id":c,"command":[]})),
        ("chat.command", json!({"chat_id":c,"command":{}})),
        ("chat.command", json!({"chat_id":c,"command":{"command":7}})),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"delete"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"Send","text":"hi"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"stop"},"extra":1}),
        ),
        (
            "chat.command",
            json!({"chat_id":"x","command":{"command":"stop"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"stop","force":true}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"interrupt","text":"x"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"compact","text":null}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"send"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"send","text":null}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"send","text":7}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"send","text":""}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"send","text":" \n\t "}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"send","text":long(65537)}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"send","text":"hi","extra":1}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"approve","request_id":"r"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"approve","decision":"accept"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"approve","request_id":"r","decision":"allow"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"approve","request_id":"r","decision":"Accept"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"approve","request_id":"","decision":"accept"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"approve","request_id":long(201),"decision":"accept"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"approve","request_id":"a\nb","decision":"accept"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"approve","request_id":7,"decision":"accept"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"answer","request_id":"r"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"answer","request_id":"r","answers":[]}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"answer","request_id":"r","answers":"yes"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"answer","request_id":"r","answers":["yes"]}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"answer","request_id":"r","answers":[[1]]}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"answer","request_id":"r","answers":[[null]]}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"answer","request_id":"r","answers":vec![vec!["a"]; 17]}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"answer","request_id":"r","answers":[vec!["a"; 65]]}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"answer","request_id":"r","answers":[[long(8193)]]}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"answer","request_id":"r","answers":vec![vec![long(8192)]; 9]}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"configure"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"configure","model":null}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"configure","model":" "}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"configure","model":long(101)}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"configure","effort":long(33)}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"configure","approval_mode":"auto-edit"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"configure","approval_mode":"yolo"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"configure","title":"x"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"configure","fast":"on"}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"configure","fast":null}}),
        ),
        (
            "chat.command",
            json!({"chat_id":c,"command":{"command":"send","text":"hi","fast":true}}),
        ),
        // Stop.
        ("chat.stop", json!({})),
        ("chat.stop", json!(null)),
        ("chat.stop", json!({"chat_id":null})),
        ("chat.stop", json!({"chat_id":"abcdefgh"})),
        ("chat.stop", json!({"chat_id":&c[..8]})),
        ("chat.stop", json!({"chat_id":c,"force":true})),
        ("chat.stop", json!({"id":c})),
    ];
    for (method, params) in bad {
        let response = f.call(method, params.clone()).await;
        assert_eq!(
            code(&response),
            "invalid_request",
            "{method} {params}: {response}"
        );
    }
}

#[tokio::test]
async fn the_limits_are_inclusive_and_what_is_blank_or_absent_is_left_out() {
    let f = Fixture::new();
    f.says("list.json", &json!([]));
    f.says(
        "create.json",
        &json!({
            "id": f.chat, "provider": "claude", "project_id": f.project, "worktree_id": f.worktree,
            "cwd": "/x", "title": "t", "created_at_unix": 1u64, "approval_mode": "supervised",
            "model": "m".repeat(100), "effort": "e".repeat(32), "state": {"state": "starting"}
        }),
    );
    let created = f
        .call(
            "chat.create",
            json!({"provider":"claude","project_id":f.project,"model":"m".repeat(100),
                   "effort":"e".repeat(32),"title":"t".repeat(200)}),
        )
        .await;
    assert_eq!(created["ok"], true, "{created}");
    // A blank value is the same as none.
    let created = f
        .call(
            "chat.create",
            json!({"provider":"claude","project_id":f.project,"model":" ","effort":"","title":"  "}),
        )
        .await;
    assert_eq!(code(&created), "", "{created}");
    let sent = f.calls_of("chat", "new");
    assert_eq!(
        sent[1],
        words(&[
            "chat",
            "new",
            "--provider",
            "claude",
            "--project",
            &f.project,
            "--mode",
            "supervised",
            "--json"
        ])
    );
    // Text of the maximum size, and a request id of the maximum size.
    for command in [
        json!({"command":"send","text":"x".repeat(65536)}),
        json!({"command":"send","text":"é".repeat(32768)}),
        json!({"command":"approve","request_id":"r".repeat(200),"decision":"cancel"}),
        json!({"command":"answer","request_id":"r","answers":vec![vec!["a".repeat(8192)]; 8]}),
        json!({"command":"answer","request_id":"r","answers":[[], ["a"]]}),
    ] {
        let sent = f
            .call("chat.command", json!({"chat_id":f.chat,"command":command}))
            .await;
        assert_eq!(sent["ok"], true, "{sent}");
    }
    // Waits and page sizes at both ends.
    f.says("events.json", &f.page(&[], 5, false));
    for (since, wait, max) in [(5, 0, 1), (5, 25000, 2000)] {
        let page = f
            .call(
                "chat.events",
                json!({"chat_id":f.chat,"since":since,"wait_ms":wait,"max_events":max}),
            )
            .await;
        assert_eq!(page["ok"], true, "{page}");
    }
}

#[tokio::test]
async fn every_value_is_its_own_argument_and_the_cli_gets_json() {
    let f = Fixture::new();
    // The list: no project, then one.
    f.says("list.json", &json!([f.info(&f.chat)]));
    let listed = f.call("chats.list", json!({})).await;
    assert_eq!(listed["ok"], true, "{listed}");
    f.call("chats.list", json!({"project_id":f.project})).await;
    assert_eq!(
        f.chat_calls(),
        vec![
            words(&["chat", "list", "--json"]),
            words(&["chat", "list", "--project", &f.project, "--json"]),
        ]
    );
    assert_eq!(
        f.calls_of("project", "show"),
        vec![words(&["project", "show", &f.project, "--json"])]
    );

    // Creation: free text stays one argument, in the `--name=VALUE` form, whatever it looks like.
    let created = |title: &str| {
        let mut info = f.info(&f.chat);
        info["title"] = json!(title);
        info["provider"] = json!("claude");
        info["approval_mode"] = json!("auto_edit");
        info["model"] = json!("opus");
        info["effort"] = json!("high");
        info
    };
    for title in [
        "Fix the build",
        "--worktree",
        "-rf; rm -rf / && $(touch pwned) `id` \"quoted\" 'single' \\ \n".trim_end_matches('\n'),
        "emoji \u{1f680} \u{e9}",
    ] {
        f.says("create.json", &created(title));
        let response = f
            .call(
                "chat.create",
                json!({"provider":"claude","worktree_id":f.worktree,"approval_mode":"auto_edit",
                       "model":"opus","effort":"high","title":title}),
            )
            .await;
        assert_eq!(response["ok"], true, "{title}: {response}");
        assert_eq!(
            f.calls_of("chat", "new").last().unwrap(),
            &words(&[
                "chat",
                "new",
                "--provider",
                "claude",
                "--worktree",
                &f.worktree,
                "--mode",
                "auto-edit",
                "--model=opus",
                "--effort=high",
                &format!("--title={title}"),
                "--json"
            ])
        );
    }
    assert!(!f.stub.path().join("pwned").exists());
    for (wire, cli) in [
        ("supervised", "supervised"),
        ("auto_edit", "auto-edit"),
        ("full", "full"),
        ("plan", "plan"),
    ] {
        let mut info = created("t");
        info["approval_mode"] = json!(wire);
        f.says("create.json", &info);
        let response = f
            .call(
                "chat.create",
                json!({"provider":"claude","worktree_id":f.worktree,"approval_mode":wire,
                       "model":"opus","effort":"high"}),
            )
            .await;
        assert_eq!(response["ok"], true, "{wire}: {response}");
        let call = f.calls_of("chat", "new").pop().unwrap();
        assert_eq!(
            call[call.iter().position(|a| a == "--mode").unwrap() + 1],
            cli
        );
    }

    // Events: the page size is the reply's less a margin; the plain limit here.
    f.says("events.json", &f.page(&[], 9, false));
    f.call(
        "chat.events",
        json!({"chat_id":f.chat,"since":9,"wait_ms":0,"max_events":123}),
    )
    .await;
    f.call(
        "chat.events",
        json!({"chat_id":f.chat,"since":9,"wait_ms":25000}),
    )
    .await;
    assert_eq!(
        f.calls_of("chat", "events"),
        vec![
            words(&[
                "chat",
                "events",
                &f.chat,
                "--since",
                "9",
                "--wait-ms",
                "0",
                "--max",
                "123",
                "--max-bytes",
                &(MAX_PLAINTEXT - 1024).to_string(),
                "--json"
            ]),
            words(&[
                "chat",
                "events",
                &f.chat,
                "--since",
                "9",
                "--wait-ms",
                "25000",
                "--max",
                "500",
                "--max-bytes",
                &(MAX_PLAINTEXT - 1024).to_string(),
                "--json"
            ]),
        ]
    );
    // A session that deflates is given a page of up to 2 MiB.
    f.call_deflating(
        "chat.events",
        json!({"chat_id":f.chat,"since":9,"wait_ms":0}),
    )
    .await;
    let last = f.calls_of("chat", "events").pop().unwrap();
    assert_eq!(
        last[last.iter().position(|a| a == "--max-bytes").unwrap() + 1],
        (link::MAX_INFLATED - 1024).to_string()
    );

    // Commands: the validated command as compact JSON in one argument, the text
    // untouched by any shell. The connector's own rebuild drops nothing it accepted.
    let text = "run `ls`; echo $HOME && --json \"quoted\" \n second line \u{1f680}";
    for command in [
        json!({"command":"send","text":text}),
        json!({"command":"interrupt"}),
        json!({"command":"compact"}),
        json!({"command":"stop"}),
        json!({"command":"approve","request_id":"perm_1","decision":"accept_for_session"}),
        json!({"command":"answer","request_id":"q-1","answers":[["Yes"],["free text","and more"],[]]}),
        json!({"command":"configure","model":"opus","effort":"low","approval_mode":"plan"}),
        json!({"command":"configure","approval_mode":"full"}),
        json!({"command":"configure","fast":true}),
        json!({"command":"configure","model":"gpt-5.5","effort":"low","fast":false}),
    ] {
        let response = f
            .call("chat.command", json!({"chat_id":f.chat,"command":command}))
            .await;
        assert_eq!(response["result"], json!({"status":"ok"}), "{response}");
        let call = f.calls_of("chat", "command").pop().unwrap();
        assert_eq!(call.len(), 6, "{call:?}");
        assert_eq!(
            call[..4],
            words(&["chat", "command", &f.chat, "--command-json"])[..]
        );
        assert_eq!(call[5], "--json");
        assert_eq!(serde_json::from_str::<Value>(&call[4]).unwrap(), command);
        assert!(!call[4].contains('\n'), "compact JSON is one line");
    }
    let stopped = f.call("chat.stop", json!({"chat_id":f.chat})).await;
    assert_eq!(stopped["result"], json!({"status":"stopped"}), "{stopped}");
    assert_eq!(
        f.calls_of("chat", "stop"),
        vec![words(&["chat", "stop", &f.chat, "--json"])]
    );
}

#[tokio::test]
async fn the_command_that_reaches_the_cli_holds_only_what_was_validated() {
    // Nothing the connector did not check can travel in the command: it is rebuilt.
    let f = Fixture::new();
    let response = f
        .call(
            "chat.command",
            json!({"chat_id":f.chat,"command":{"command":"configure","model":"opus"}}),
        )
        .await;
    assert_eq!(response["ok"], true, "{response}");
    let call = f.calls_of("chat", "command").pop().unwrap();
    assert_eq!(call[4], r#"{"command":"configure","model":"opus"}"#);
    let call_for = |command: Value| {
        let f = &f;
        async move {
            f.call("chat.command", json!({"chat_id":f.chat,"command":command}))
                .await;
            f.calls_of("chat", "command").pop().unwrap()[4].clone()
        }
    };
    assert_eq!(
        call_for(json!({"command":"send","text":"hi"})).await,
        r#"{"command":"send","text":"hi"}"#
    );
    assert_eq!(
        call_for(json!({"command":"stop"})).await,
        r#"{"command":"stop"}"#
    );
}

#[tokio::test]
async fn the_result_is_the_chat_the_cli_printed_and_a_list_holds_only_chats() {
    let f = Fixture::new();
    let other = new_uuid();
    f.says("list.json", &json!([f.info(&f.chat), f.info(&other)]));
    let listed = f.call("chats.list", json!({})).await;
    assert_eq!(
        listed["result"],
        json!({"chats":[f.info(&f.chat), f.info(&other)]}),
        "{listed}"
    );
    // Nothing the desktop added is left out: the phone decodes what it knows.
    let mut richer = f.info(&f.chat);
    richer["future_field"] = json!({"a": [1, 2]});
    f.says("list.json", &json!([richer.clone()]));
    let listed = f.call("chats.list", json!({})).await;
    assert_eq!(listed["result"]["chats"][0], richer);

    // A project's list holds that project's chats and nothing else.
    f.says("list.json", &json!([f.info(&f.chat)]));
    let mine = f.call("chats.list", json!({"project_id":f.project})).await;
    assert_eq!(
        mine["result"]["chats"].as_array().unwrap().len(),
        1,
        "{mine}"
    );
    let mut elsewhere = f.info(&other);
    elsewhere["project_id"] = json!(new_uuid());
    f.says("list.json", &json!([f.info(&f.chat), elsewhere]));
    let mixed = f.call("chats.list", json!({"project_id":f.project})).await;
    assert_eq!(code(&mixed), "cli_error", "{mixed}");

    // Things that are not chats.
    let mut no_id = f.info(&f.chat);
    no_id.as_object_mut().unwrap().remove("id");
    let mut bad_id = f.info(&f.chat);
    bad_id["id"] = json!("not-a-uuid");
    let mut bad_provider = f.info(&f.chat);
    bad_provider["provider"] = json!("gemini");
    let mut bad_state = f.info(&f.chat);
    bad_state["state"] = json!("idle");
    let mut no_time = f.info(&f.chat);
    no_time.as_object_mut().unwrap().remove("created_at_unix");
    for bad in [
        json!({}),
        json!("chat"),
        no_id,
        bad_id,
        bad_provider,
        bad_state,
        no_time,
    ] {
        f.says("list.json", &json!([bad.clone()]));
        let response = f.call("chats.list", json!({})).await;
        assert_eq!(code(&response), "cli_error", "{bad}: {response}");
    }
    f.set("list.json", "{\"chats\":[]}");
    assert_eq!(code(&f.call("chats.list", json!({})).await), "cli_error");
    f.set("list.json", "no chats today");
    assert_eq!(code(&f.call("chats.list", json!({})).await), "cli_error");

    // Creation answers with the chat, whatever state it is in: a chat whose agent did not
    // start is the answer, so the phone does not start another.
    let mut failed = f.info(&f.chat);
    failed["state"] = json!({"state":"failed","message":"codex is not installed"});
    f.says("create.json", &failed);
    let created = f
        .call(
            "chat.create",
            json!({"provider":"codex","project_id":f.project}),
        )
        .await;
    assert_eq!(created["result"], json!({"chat": failed}), "{created}");
}

#[tokio::test]
async fn a_chat_that_was_not_asked_for_is_stopped_rather_than_left_running() {
    let f = Fixture::new();
    let stray = new_uuid();
    let ask = json!({"provider":"codex","project_id":f.project,"approval_mode":"plan",
                     "model":"gpt","effort":"low"});
    let right = |f: &Fixture| {
        let mut info = f.info(&stray);
        info["approval_mode"] = json!("plan");
        info["model"] = json!("gpt");
        info["effort"] = json!("low");
        info
    };
    // The asked-for chat is accepted.
    f.says("create.json", &right(&f));
    assert_eq!(f.call("chat.create", ask.clone()).await["ok"], true);
    assert!(f.calls_of("chat", "stop").is_empty());

    let mut wrong_provider = right(&f);
    wrong_provider["provider"] = json!("claude");
    let mut wrong_project = right(&f);
    wrong_project["project_id"] = json!(new_uuid());
    let mut wrong_mode = right(&f);
    wrong_mode["approval_mode"] = json!("full");
    let mut wrong_model = right(&f);
    wrong_model["model"] = json!("other");
    let mut no_effort = right(&f);
    no_effort.as_object_mut().unwrap().remove("effort");
    for (n, bad) in [
        wrong_provider,
        wrong_project,
        wrong_mode,
        wrong_model,
        no_effort,
    ]
    .into_iter()
    .enumerate()
    {
        f.says("create.json", &bad);
        let response = f.call("chat.create", ask.clone()).await;
        assert_eq!(code(&response), "cli_error", "{bad}: {response}");
        for _ in 0..300 {
            if f.calls_of("chat", "stop").len() > n {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(
            f.calls_of("chat", "stop").len(),
            n + 1,
            "{bad}: the stray chat was stopped"
        );
        assert_eq!(f.calls_of("chat", "stop")[n][2], stray);
    }
    // A worktree chat is judged by its worktree.
    f.says("create.json", &f.info(&stray));
    let mut wrong = f.info(&stray);
    wrong["worktree_id"] = json!(new_uuid());
    f.says("create.json", &wrong);
    let response = f
        .call(
            "chat.create",
            json!({"provider":"codex","worktree_id":f.worktree}),
        )
        .await;
    assert_eq!(code(&response), "cli_error", "{response}");
    // Not a chat at all: nothing to stop.
    let before = f.calls_of("chat", "stop").len();
    f.set("create.json", "created chat 1234");
    let response = f
        .call(
            "chat.create",
            json!({"provider":"codex","project_id":f.project}),
        )
        .await;
    assert_eq!(code(&response), "cli_error", "{response}");
    f.says("create.json", &json!({"id": "not-a-uuid"}));
    let response = f
        .call(
            "chat.create",
            json!({"provider":"codex","project_id":f.project}),
        )
        .await;
    assert_eq!(code(&response), "cli_error", "{response}");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let stops = f.calls_of("chat", "stop").len();
    // Only the worktree case above (and none of these) may have added one.
    assert!(stops <= before + 1, "{:?}", f.calls_of("chat", "stop"));
}

#[tokio::test]
async fn fast_mode_is_a_flag_of_the_new_chat_and_a_chat_that_ignored_it_is_stopped() {
    let f = Fixture::new();
    let stray = new_uuid();
    let fast_chat = |fast: bool| {
        let mut info = f.info(&stray);
        info["fast"] = json!(fast);
        info["model"] = json!("gpt-5.5");
        info
    };
    // Asked for: the CLI is told with `--fast`, after the free-text values.
    f.says("create.json", &fast_chat(true));
    let created = f
        .call(
            "chat.create",
            json!({"provider":"codex","project_id":f.project,"model":"gpt-5.5","fast":true}),
        )
        .await;
    assert_eq!(created["result"]["chat"]["fast"], true, "{created}");
    assert_eq!(
        f.calls_of("chat", "new").pop().unwrap(),
        words(&[
            "chat",
            "new",
            "--provider",
            "codex",
            "--project",
            &f.project,
            "--mode",
            "supervised",
            "--model=gpt-5.5",
            "--fast",
            "--json"
        ])
    );
    // Not asked for, or turned off: no flag, and a chat that is not fast is what was asked.
    for ask in [json!({}), json!({"fast":false})] {
        f.says("create.json", &fast_chat(false));
        let mut params = json!({"provider":"codex","project_id":f.project,"model":"gpt-5.5"});
        params
            .as_object_mut()
            .unwrap()
            .extend(ask.as_object().unwrap().clone());
        let created = f.call("chat.create", params).await;
        assert_eq!(created["ok"], true, "{created}");
        assert!(
            !f.calls_of("chat", "new")
                .pop()
                .unwrap()
                .contains(&"--fast".to_owned())
        );
    }
    // A chat that is not in fast mode is not the chat that was asked for.
    f.says("create.json", &fast_chat(false));
    let created = f
        .call(
            "chat.create",
            json!({"provider":"codex","project_id":f.project,"model":"gpt-5.5","fast":true}),
        )
        .await;
    assert_eq!(code(&created), "cli_error", "{created}");
    for _ in 0..300 {
        if !f.calls_of("chat", "stop").is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        f.calls_of("chat", "stop").len(),
        1,
        "the stray chat was stopped"
    );
}

#[tokio::test]
async fn a_models_event_and_the_fast_flag_pass_through_a_page_as_the_cli_printed_them() {
    let f = Fixture::new();
    let models = json!({"seq": 12, "event": {
        "event": "models",
        "models": [{
            "id": "gpt-5.5", "name": "GPT-5.5", "description": "Frontier",
            "efforts": ["low", "medium", "high"], "default_effort": "medium",
            "supports_fast": true, "is_default": true
        }, {"id": "default", "name": "Default"}]
    }});
    let mut info = f.info(&f.chat);
    info["fast"] = json!(true);
    let changed = json!({"seq": 13, "event": {"event": "info", "info": info}});
    let page = f.page(&[models, changed], 13, false);
    f.says("events.json", &page);
    let response = f
        .call(
            "chat.events",
            json!({"chat_id":f.chat,"since":11,"wait_ms":0}),
        )
        .await;
    assert_eq!(response["result"], page, "{response}");
    // And so does a chat that has it, in a list.
    f.says("list.json", &json!([info]));
    let listed = f.call("chats.list", json!({})).await;
    assert_eq!(listed["result"]["chats"][0]["fast"], true, "{listed}");
}

#[tokio::test]
async fn the_target_must_exist_under_exactly_the_id_given() {
    let f = Fixture::new();
    let other = new_uuid();
    let creations = |f: &Fixture| f.calls_of("chat", "new").len();
    for (params, kind) in [
        (
            json!({"provider":"codex","project_id":f.project}),
            "project",
        ),
        (
            json!({"provider":"claude","worktree_id":f.worktree}),
            "worktree",
        ),
    ] {
        // The desktop does not know the id.
        f.set("show.error", &format!("No {kind} matches 'x'"));
        let response = f.call("chat.create", params.clone()).await;
        assert_eq!(code(&response), "not_found", "{kind}: {response}");
        assert_eq!(
            message(&response),
            format!("{kind} not found on the desktop")
        );
        assert_eq!(creations(&f), 0);
        // It knows something else by that name: a project name or a branch that spells the id.
        f.unset("show.error");
        f.set("show.id", &other);
        let response = f.call("chat.create", params.clone()).await;
        assert_eq!(code(&response), "not_found", "{kind}: {response}");
        assert_eq!(creations(&f), 0);
        // Any other failure of the look-up is the CLI's, and still starts nothing.
        f.unset("show.id");
        f.set(
            "show.error",
            "More than one project matches 'x'; use its UUID",
        );
        let response = f.call("chat.create", params).await;
        assert_eq!(code(&response), "cli_error", "{kind}: {response}");
        assert_eq!(creations(&f), 0);
        f.unset("show.error");
    }
    // The same look-up guards the list of a project's chats.
    f.set("show.id", &other);
    let response = f.call("chats.list", json!({"project_id":f.project})).await;
    assert_eq!(code(&response), "not_found", "{response}");
    assert!(f.calls_of("chat", "list").is_empty());
}

#[tokio::test]
async fn a_chat_being_made_is_finished_when_the_request_is_dropped() {
    // The connector drops a request's task when its connection ends (relay error,
    // revocation): the CLI must not be killed between the host writing the chat down
    // and the answer.
    let f = Arc::new(Fixture::new());
    f.says("create.json", &f.info(&f.chat));
    f.set("create.delay", "0.6");
    let task = {
        let f = f.clone();
        tokio::spawn(async move {
            f.call(
                "chat.create",
                json!({"provider":"codex","project_id":f.project}),
            )
            .await
        })
    };
    for _ in 0..500 {
        if !f.calls_of("chat", "new").is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(f.calls_of("chat", "new").len(), 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
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
    let chat = f.chat.clone();
    let cases: Vec<(&str, &str, String)> = vec![
        (
            "Unknown chat 00000000-0000-4000-8000-000000000000",
            "not_found",
            "chat not found on the desktop".into(),
        ),
        (
            "unknown chat 00000000-0000-4000-8000-000000000000",
            "not_found",
            "chat not found on the desktop".into(),
        ),
        (
            "invalid_request: since is beyond the end of the chat",
            "invalid_request",
            "since is beyond the end of the chat".into(),
        ),
        (
            "codex is not installed or is not on PATH",
            "harness_unavailable",
            "codex is not installed or is not on PATH".into(),
        ),
        (
            "claude is not installed or is not on PATH",
            "harness_unavailable",
            "claude is not installed or is not on PATH".into(),
        ),
        (
            "Cua Driver is not installed. Open RiWork Settings, or run `riwork setup`.",
            "harness_unavailable",
            "Cua Driver is not installed. Open RiWork Settings, or run `riwork setup`.".into(),
        ),
        // The words of the host, as they are.
        (
            "the chat is stopped; send a message to resume it",
            "cli_error",
            "the chat is stopped; send a message to resume it".into(),
        ),
        (
            "no pending approval perm_1",
            "cli_error",
            "no pending approval perm_1".into(),
        ),
        // A look-alike is not the installation error.
        (
            "grok is not installed or is not on PATH",
            "cli_error",
            "grok is not installed or is not on PATH".into(),
        ),
        (
            "something about codex is not installed or is not on PATH",
            "cli_error",
            "something about codex is not installed or is not on PATH".into(),
        ),
    ];
    for (method, params, error) in [
        (
            "chat.command",
            json!({"chat_id":chat,"command":{"command":"send","text":"hi"}}),
            "command.error",
        ),
        (
            "chat.events",
            json!({"chat_id":chat,"since":0,"wait_ms":0}),
            "events.error",
        ),
        ("chat.stop", json!({"chat_id":chat}), "stop.error"),
        (
            "chat.create",
            json!({"provider":"codex","project_id":f.project}),
            "create.error",
        ),
        ("chats.list", json!({}), "list.error"),
    ] {
        for (line, expected, text) in &cases {
            f.set(error, line);
            let response = f.call(method, params.clone()).await;
            assert_eq!(code(&response), *expected, "{method}: {line}: {response}");
            assert_eq!(message(&response), text, "{method}: {line}");
        }
        // Only the first line decides, so a later one that looks like a known error is not
        // mistaken for it.
        f.set(error, "boom\nUnknown chat x");
        let response = f.call(method, params.clone()).await;
        assert_eq!(code(&response), "cli_error", "{method}: {response}");
        assert_eq!(message(&response), "boom");
        f.unset(error);
    }
    // The host being down is not an error for a stop: nothing runs.
    f.set(
        "stop.error",
        "No chat host is running, so no chat is running",
    );
    let stopped = f.call("chat.stop", json!({"chat_id":chat})).await;
    assert_eq!(stopped["result"], json!({"status":"stopped"}), "{stopped}");
    // And it is only that sentence, at the start of the error.
    f.set("stop.error", "tmux said: No chat host is running");
    assert_eq!(
        code(&f.call("chat.stop", json!({"chat_id":chat})).await),
        "cli_error"
    );
}

async fn listed(f: &Fixture) -> Value {
    f.call("chats.list", json!({})).await
}

#[tokio::test]
async fn a_cli_without_chats_is_told_apart_and_asked_again_until_it_says_yes() {
    let f = Fixture::new();
    let capabilities = |f: &Fixture| f.calls_of("capabilities", "--json").len();
    let too_old = |response: &Value| {
        assert_eq!(code(response), "cli_error", "{response}");
        assert!(message(response).contains("update RiWork"), "{response}");
    };
    for says in [
        "{\"v\":1}",
        "{\"v\":1,\"chat\":false}",
        "{\"v\":1,\"chat\":\"true\"}",
        "{\"v\":2,\"chat\":true}",
        "chat yes",
        "",
    ] {
        f.set("capabilities.out", says);
        too_old(&listed(&f).await);
    }
    f.unset("capabilities.out");
    f.set("capabilities.unknown", "");
    too_old(&listed(&f).await);
    for method in [
        ("chat.stop", json!({"chat_id":f.chat})),
        (
            "chat.events",
            json!({"chat_id":f.chat,"since":0,"wait_ms":0}),
        ),
        (
            "chat.command",
            json!({"chat_id":f.chat,"command":{"command":"stop"}}),
        ),
        (
            "chat.create",
            json!({"provider":"codex","project_id":f.project}),
        ),
    ] {
        too_old(&f.call(method.0, method.1).await);
    }
    // No chat command was run, and a creation looked nothing up either.
    assert!(f.chat_calls().is_empty());
    assert!(f.calls_of("project", "show").is_empty());
    // A CLI updated while the connector runs is believed at once, and a yes is kept.
    f.unset("capabilities.unknown");
    let before = capabilities(&f);
    assert_eq!(listed(&f).await["ok"], true);
    assert_eq!(capabilities(&f), before + 1);
    f.set("capabilities.unknown", "");
    assert_eq!(listed(&f).await["ok"], true);
    assert_eq!(listed(&f).await["ok"], true);
    assert_eq!(capabilities(&f), before + 1, "a yes is remembered");
    // What `ready` says is the same question.
    assert!(f.rpc.chat_supported().await);
    let fresh = Fixture::new();
    assert!(fresh.rpc.chat_supported().await);
    fresh.set("capabilities.out", "{\"v\":1,\"chat\":false}");
    let fresher = Rpc::new(fresh.rpc.cli.clone(), fresh.rpc.storage.clone());
    assert!(!fresher.chat_supported().await);
    let missing = Rpc::new("/nonexistent/riwork".into(), fresh.rpc.storage.clone());
    assert!(!missing.chat_supported().await);
}

#[tokio::test]
async fn an_events_page_is_what_the_cli_collected_checked_against_the_request() {
    let f = Fixture::new();
    let events = |from: u64, count: u64| -> Vec<Value> {
        (from..from + count)
            .map(|seq| Fixture::event(seq, "hello"))
            .collect()
    };
    let ask = |since: u64, max: u64| {
        let (f, chat) = (&f, f.chat.clone());
        async move {
            f.call(
                "chat.events",
                json!({"chat_id":chat,"since":since,"wait_ms":0,"max_events":max}),
            )
            .await
        }
    };
    // A page, as it is.
    let page = f.page(&events(11, 3), 13, false);
    f.says("events.json", &page);
    let response = ask(10, 500).await;
    assert_eq!(response["result"], page, "{response}");
    // Nothing new: the cursor stays.
    let quiet = f.page(&[], 10, false);
    f.says("events.json", &quiet);
    assert_eq!(ask(10, 500).await["result"], quiet);
    // More to come.
    let cut = f.page(&events(11, 2), 12, true);
    f.says("events.json", &cut);
    assert_eq!(ask(10, 2).await["result"], cut);
    // Events the desktop does not hand out are not passed on (only `seq` and `event`), and what
    // is inside an event is the desktop's own, unknown parts included.
    let mut odd = Fixture::event(11, "x");
    odd["event"]["from_the_future"] = json!([1, {"a": null}]);
    odd["event"]["event"] = json!("something_new");
    f.says("events.json", &f.page(&[odd.clone()], 11, false));
    assert_eq!(ask(10, 500).await["result"]["events"][0], odd);

    // A newer CLI may print more than the contract has: only the contract's fields go on.
    f.says(
        "events.json",
        &json!({"chat_id":f.chat,"events":[{"seq":11,"event":{"event":"state"},"at":1}],
                "next":11,"more":false,"skipped":0}),
    );
    assert_eq!(
        ask(10, 500).await["result"],
        json!({"chat_id":f.chat,"events":[{"seq":11,"event":{"event":"state"}}],
               "next":11,"more":false})
    );

    let two = events(11, 2);
    let bad = |page: Value| {
        let (f, ask) = (&f, &ask);
        async move {
            f.says("events.json", &page);
            let response = ask(10, 500).await;
            assert_eq!(code(&response), "invalid_reply", "{page}: {response}");
        }
    };
    // Another chat's page.
    let mut other_chat = f.page(&two, 12, false);
    other_chat["chat_id"] = json!(new_uuid());
    bad(other_chat).await;
    // Events at or before `since`, repeated, out of order.
    bad(f.page(&events(10, 2), 11, false)).await;
    bad(f.page(&[two[0].clone(), two[0].clone()], 11, false)).await;
    bad(f.page(&[two[1].clone(), two[0].clone()], 12, false)).await;
    // More events than asked for.
    f.says("events.json", &f.page(&events(11, 3), 13, false));
    let response = ask(10, 2).await;
    assert_eq!(code(&response), "invalid_reply", "{response}");
    // A cursor before what the page holds, or a cut page that does not move on.
    bad(f.page(&two, 11, false)).await;
    bad(f.page(&[], 9, false)).await;
    bad(f.page(&[], 10, true)).await;
    // Entries that are not `{seq, event}`.
    bad(f.page(&[json!({"seq":11})], 11, false)).await;
    bad(f.page(&[json!({"event":{"event":"state"}})], 11, false)).await;
    bad(f.page(&[json!({"seq":"11","event":{"event":"state"}})], 11, false)).await;
    bad(f.page(&[json!({"seq":11,"event":{}})], 11, false)).await;
    bad(f.page(&[json!({"seq":11,"event":"state"})], 11, false)).await;
    bad(f.page(&[json!(11)], 11, false)).await;
    // A page that is not one.
    bad(json!({"chat_id":f.chat,"events":[],"next":10})).await;
    bad(json!({"chat_id":f.chat,"events":[],"more":false})).await;
    bad(json!({"chat_id":f.chat,"next":10,"more":false})).await;
    bad(json!({"chat_id":f.chat,"events":{},"next":10,"more":false})).await;
    bad(json!({"chat_id":f.chat,"events":[],"next":"10","more":false})).await;
    bad(json!({"chat_id":f.chat,"events":[],"next":10,"more":"no"})).await;
    bad(json!([])).await;
    f.set("events.json", "not json");
    assert_eq!(code(&ask(10, 500).await), "cli_error");
}

#[tokio::test]
async fn a_page_is_cut_to_the_events_that_fit_the_reply() {
    let f = Fixture::new();
    // The CLI keeps a page within the size it is given, a margin short of a frame. A page
    // that is not (a CLI that miscounts its envelope) is cut here, to the first half of its
    // events, and says so.
    let mut events: Vec<Value> = Vec::new();
    while f.page(&events, 0, false).to_string().len() < MAX_PLAINTEXT - 3000 {
        let seq = events.len() as u64 + 1;
        events.push(Fixture::event(seq, &"the same sentence again. ".repeat(36)));
    }
    // The last one pads the page to 20 bytes under a frame: the CLI's part fits, the
    // reply's own fields do not.
    let seq = events.len() as u64 + 1;
    events.push(Fixture::event(seq, ""));
    let short = f.page(&events, seq, false).to_string().len();
    events[seq as usize - 1] = Fixture::event(seq, &"x".repeat(MAX_PLAINTEXT - 20 - short));
    let count = events.len();
    f.says("events.json", &f.page(&events, count as u64, false));
    let params = json!({"chat_id":f.chat,"since":0,"wait_ms":0});

    // Without compression, a frame holds 128 KiB of JSON, the reply's own fields included.
    let plain = f.call("chat.events", params.clone()).await;
    assert_eq!(
        plain["ok"],
        true,
        "{}",
        &plain.to_string()[..300.min(plain.to_string().len())]
    );
    let held = plain["result"]["events"].as_array().unwrap();
    assert_eq!(held.len(), count / 2);
    assert_eq!(plain["result"]["more"], true);
    assert_eq!(plain["result"]["next"], held.len() as u64);
    assert_eq!(held[..], events[..held.len()], "the first events, in order");
    assert!(serde_json::to_vec(&plain).unwrap().len() <= MAX_PLAINTEXT);

    // A page that leaves room is not touched.
    let fits = f.page(&events[..count - 20], (count - 20) as u64, false);
    f.says("events.json", &fits);
    assert_eq!(f.call("chat.events", params.clone()).await["result"], fits);
    f.says("events.json", &f.page(&events, count as u64, false));

    // A session that deflates takes all of it: the text compresses far below a frame.
    let deflated = f.call_deflating("chat.events", params.clone()).await;
    assert_eq!(
        deflated["result"]["events"].as_array().unwrap().len(),
        count
    );
    assert_eq!(deflated["result"]["more"], false);
    assert_eq!(deflated["result"]["next"], count as u64);
    let encoded =
        link::encode_reply(&deflated, std::time::Instant::now(), true, MAX_PLAINTEXT).unwrap();
    assert!(encoded.deflated && encoded.json_bytes > MAX_PLAINTEXT);

    // Noise does not: the page is cut until its deflated form fits one frame, and the cut
    // page is exactly the one a phone would ask for next.
    let noisy: Vec<Value> = (1..=400)
        .map(|seq| Fixture::event(seq, &noise(2000, seq * 7919 + 1)))
        .collect();
    f.says("events.json", &f.page(&noisy, 400, false));
    let cut = f.call_deflating("chat.events", params.clone()).await;
    assert_eq!(cut["ok"], true, "{cut}");
    let held = cut["result"]["events"].as_array().unwrap();
    assert!(!held.is_empty() && held.len() < 400, "{}", held.len());
    assert_eq!(cut["result"]["more"], true);
    assert_eq!(cut["result"]["next"], held.last().unwrap()["seq"]);
    link::encode_reply(&cut, std::time::Instant::now(), true, MAX_PLAINTEXT)
        .expect("the cut page fits a frame, deflated");
    assert_eq!(held[..], noisy[..held.len()]);

    // One event that is more than a frame by itself is cut until it fits, so that the phone
    // can go on past it: a page that never ends would be a chat nobody can read. (The CLI
    // keeps a page within the reply limit, 128 KiB or 2 MiB, and noise does not deflate.)
    for (size, deflate) in [(0, false), (400_000, true)] {
        let huge = if deflate {
            Fixture::event(1, &noise(size, 12345))
        } else {
            // A page 30 bytes under a frame on its own: the reply's fields are what is over.
            let bare = f.page(&[Fixture::event(1, "")], 1, false).to_string().len();
            Fixture::event(1, &"y".repeat(MAX_PLAINTEXT - 30 - bare))
        };
        let text = huge["event"]["item"]["body"]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        f.says("events.json", &f.page(&[huge], 1, false));
        let cut = if deflate {
            f.call_deflating("chat.events", params.clone()).await
        } else {
            f.call("chat.events", params.clone()).await
        };
        assert_eq!(cut["ok"], true, "{size}: {cut}");
        let held = cut["result"]["events"].as_array().unwrap();
        assert_eq!(held.len(), 1, "{cut}");
        let kept = held[0]["event"]["item"]["body"]["text"].as_str().unwrap();
        assert!(kept.ends_with('\u{2026}') && kept.len() < text.len());
        assert!(text.starts_with(kept.trim_end_matches('\u{2026}')));
        assert_eq!(
            (cut["result"]["next"].clone(), cut["result"]["more"].clone()),
            (json!(1), json!(false))
        );
        link::encode_reply(&cut, std::time::Instant::now(), deflate, MAX_PLAINTEXT)
            .expect("the cut event fits a frame");
    }
}

#[tokio::test]
async fn a_cli_that_prints_more_than_a_reply_may_hold_is_too_large() {
    let f = Fixture::new();
    f.set("events.json", &" ".repeat(MAX_PLAINTEXT + 10));
    let response = f
        .call(
            "chat.events",
            json!({"chat_id":f.chat,"since":0,"wait_ms":0}),
        )
        .await;
    assert_eq!(code(&response), "response_too_large", "{response}");
    assert!(message(&response).contains("fewer events"), "{response}");
}

#[tokio::test]
async fn a_revoked_device_does_nothing() {
    let f = Fixture::new();
    f.rpc.storage.revoke(&f.device).unwrap();
    for (method, params) in [
        ("chats.list", json!({})),
        (
            "chat.create",
            json!({"provider":"codex","project_id":f.project}),
        ),
        (
            "chat.events",
            json!({"chat_id":f.chat,"since":0,"wait_ms":0}),
        ),
        (
            "chat.command",
            json!({"chat_id":f.chat,"command":{"command":"stop"}}),
        ),
        ("chat.stop", json!({"chat_id":f.chat})),
    ] {
        assert!(
            f.rpc.handle(&f.device, req(method, params)).await.is_err(),
            "{method}"
        );
    }
    assert!(f.chat_calls().is_empty());
    assert!(f.calls_of("project", "show").is_empty());
}

#[tokio::test]
async fn the_other_methods_are_unchanged() {
    // The chat methods are exact names.
    let f = Fixture::new();
    for method in [
        "chats.create",
        "chat.list",
        "chat",
        "chat.events.",
        "Chat.stop",
        "chat.delete",
    ] {
        let response = f.call(method, json!({})).await;
        assert_eq!(code(&response), "invalid_request", "{method}");
        assert_eq!(message(&response), "unsupported RPC method", "{method}");
    }
    assert!(f.calls().is_empty());
}

/// The real CLI in a throwaway home, behind a wrapper that keeps it off the user's real
/// data. The home is short: a Unix socket path holds 103 bytes. Its chat host is ended
/// with the test.
struct RealCli {
    dir: PathBuf,
    wrapper: PathBuf,
}
impl RealCli {
    fn new(cli: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("rwr-{}", &new_uuid()[..6]));
        let home = dir.join("home");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let cua = bin.join("cua-driver");
        std::fs::write(&cua, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&cua, std::fs::Permissions::from_mode(0o755)).unwrap();
        let wrapper = dir.join("riwork");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexport HOME='{dir}' RIWORK_HOME='{home}' RIWORK_RUNTIME_DIR='{home}/runtime' \
                 RIWORK_CUA_DRIVER='{cua}' PATH='{bin}':\"$PATH\"\nexec '{cli}' \"$@\"\n",
                dir = dir.display(),
                home = home.display(),
                cua = cua.display(),
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
        // Only the host of this test's own home.
        if let Some(pid) = std::fs::read_to_string(self.dir.join("home/run/chat.lock"))
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .status();
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
#[ignore = "requires RIWORK_TEST_CLI; isolated real CLI and chat host in a throwaway home, no provider is started"]
async fn the_real_cli_lists_creates_follows_and_stops_a_chat_through_the_rpc() {
    let cli = PathBuf::from(std::env::var_os("RIWORK_TEST_CLI").expect("set RIWORK_TEST_CLI"));
    assert!(cli.is_absolute(), "absolute CLI path required");
    let real = RealCli::new(&cli);
    let repo = real.dir.join("app");
    let other_repo = real.dir.join("other");
    let mut projects = Vec::new();
    for repo in [&repo, &other_repo] {
        std::fs::create_dir_all(repo).unwrap();
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
                    .arg(repo)
                    .args(&args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        projects.push(
            real.run(&["project", "add", repo.to_str().unwrap(), "--json"])["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }

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
    assert!(rpc.chat_supported().await, "the CLI announces chats");
    let call = |method: &str, params: Value| {
        let (rpc, device) = (&rpc, device.clone());
        let request = req(method, params);
        async move { rpc.handle(&device, request).await.unwrap() }
    };

    // Nothing yet, for the desktop and for a project.
    let none = call("chats.list", json!({})).await;
    assert_eq!(none["result"], json!({"chats":[]}), "{none}");
    let none = call("chats.list", json!({"project_id":projects[0]})).await;
    assert_eq!(none["result"], json!({"chats":[]}), "{none}");
    let missing = call("chats.list", json!({"project_id":new_uuid()})).await;
    assert_eq!(code(&missing), "not_found", "{missing}");

    // A chat is created whatever becomes of its agent (this build may have none).
    let created = call(
        "chat.create",
        json!({"provider":"codex","project_id":projects[0],"approval_mode":"plan",
               "title":"--first: a \"chat\""}),
    )
    .await;
    assert_eq!(created["ok"], true, "{created}");
    let chat = created["result"]["chat"].clone();
    let id = chat["id"].as_str().unwrap().to_owned();
    assert_eq!(chat["provider"], "codex");
    assert_eq!(chat["project_id"], projects[0].as_str());
    assert_eq!(chat["approval_mode"], "plan");
    assert_eq!(chat["title"], "--first: a \"chat\"");
    assert!(chat["state"]["state"].is_string(), "{chat}");
    let missing = call(
        "chat.create",
        json!({"provider":"claude","worktree_id":new_uuid()}),
    )
    .await;
    assert_eq!(code(&missing), "not_found", "{missing}");

    // Listed for the desktop and for its project, and for no other.
    let all = call("chats.list", json!({})).await;
    assert_eq!(all["result"]["chats"].as_array().unwrap().len(), 1, "{all}");
    assert_eq!(all["result"]["chats"][0]["id"], id.as_str());
    let mine = call("chats.list", json!({"project_id":projects[0]})).await;
    assert_eq!(mine["result"]["chats"][0]["id"], id.as_str(), "{mine}");
    let theirs = call("chats.list", json!({"project_id":projects[1]})).await;
    assert_eq!(theirs["result"], json!({"chats":[]}), "{theirs}");

    // Its history so far: numbered from 1 without gaps, the first of them the chat itself.
    let page = call("chat.events", json!({"chat_id":id,"since":0,"wait_ms":0})).await;
    assert_eq!(page["ok"], true, "{page}");
    let events = page["result"]["events"].as_array().unwrap().clone();
    assert!(!events.is_empty(), "{page}");
    for (n, entry) in events.iter().enumerate() {
        assert_eq!(entry["seq"], n as u64 + 1, "{page}");
        assert!(entry["event"]["event"].is_string(), "{page}");
    }
    assert_eq!(events[0]["event"]["event"], "info");
    assert_eq!(events[0]["event"]["info"]["id"], id.as_str());
    let next = page["result"]["next"].as_u64().unwrap();
    assert_eq!(next, events.len() as u64);
    assert_eq!(page["result"]["more"], false);
    // The cursor works: from `next` nothing is new, and the wait is what was asked for.
    let started = std::time::Instant::now();
    let quiet = call(
        "chat.events",
        json!({"chat_id":id,"since":next,"wait_ms":700}),
    )
    .await;
    assert_eq!(
        quiet["result"],
        json!({"chat_id":id,"events":[],"next":next,"more":false}),
        "{quiet}"
    );
    assert!(started.elapsed() >= std::time::Duration::from_millis(650));
    // A page can be asked for in pieces.
    let first = call(
        "chat.events",
        json!({"chat_id":id,"since":0,"wait_ms":0,"max_events":1}),
    )
    .await;
    assert_eq!(first["result"]["events"].as_array().unwrap().len(), 1);
    assert_eq!(first["result"]["next"], 1);
    assert_eq!(first["result"]["more"], next > 1, "{first}");
    // Past the end, and a chat nobody has.
    let beyond = call(
        "chat.events",
        json!({"chat_id":id,"since":next + 100,"wait_ms":0}),
    )
    .await;
    assert_eq!(code(&beyond), "invalid_request", "{beyond}");
    let unknown = call(
        "chat.events",
        json!({"chat_id":new_uuid(),"since":0,"wait_ms":0}),
    )
    .await;
    assert_eq!(code(&unknown), "not_found", "{unknown}");

    // Commands reach the host. With no agent behind this chat they are refused in the
    // host's own words; what matters here is that the answer is the connector's.
    let sent = call(
        "chat.command",
        json!({"chat_id":id,"command":{"command":"send","text":"hello"}}),
    )
    .await;
    assert!(
        sent["ok"] == true || ["cli_error", "harness_unavailable"].contains(&code(&sent)),
        "{sent}"
    );
    let interrupt = call(
        "chat.command",
        json!({"chat_id":id,"command":{"command":"interrupt"}}),
    )
    .await;
    assert!(
        interrupt["ok"] == true || code(&interrupt) == "cli_error",
        "{interrupt}"
    );
    let unknown = call(
        "chat.command",
        json!({"chat_id":new_uuid(),"command":{"command":"interrupt"}}),
    )
    .await;
    assert_eq!(code(&unknown), "not_found", "{unknown}");

    // Stopping keeps the chat and its history; stopping twice is fine.
    for _ in 0..2 {
        let stopped = call("chat.stop", json!({"chat_id":id})).await;
        assert_eq!(stopped["result"], json!({"status":"stopped"}), "{stopped}");
    }
    let unknown = call("chat.stop", json!({"chat_id":new_uuid()})).await;
    assert_eq!(code(&unknown), "not_found", "{unknown}");
    let after = call("chats.list", json!({})).await;
    assert_eq!(after["result"]["chats"][0]["state"]["state"], "stopped");
    let again = call("chat.events", json!({"chat_id":id,"since":0,"wait_ms":0})).await;
    assert!(
        again["result"]["events"].as_array().unwrap().len() >= events.len(),
        "{again}"
    );
}

#[tokio::test]
async fn a_cli_that_cannot_be_run_is_not_one_that_lacks_chats() {
    // "Update RiWork" is for a CLI that ran and does not know chats; a CLI that could not be
    // started says what is wrong with starting it.
    let f = Fixture::without_cli();
    for (method, params) in [
        ("chats.list", json!({})),
        (
            "chat.create",
            json!({"provider":"codex","project_id":f.project}),
        ),
        (
            "chat.events",
            json!({"chat_id":f.chat,"since":0,"wait_ms":0}),
        ),
        (
            "chat.command",
            json!({"chat_id":f.chat,"command":{"command":"stop"}}),
        ),
        ("chat.stop", json!({"chat_id":f.chat})),
    ] {
        let response = f.call(method, params).await;
        assert_eq!(code(&response), "cli_error", "{method}: {response}");
        assert!(
            message(&response).contains("start configured RiWork CLI")
                && !message(&response).contains("update RiWork"),
            "{method}: {response}"
        );
    }
    // And it is not remembered as anything: `ready` simply does not announce chats.
    assert!(!f.rpc.chat_supported().await);
}

#[tokio::test]
async fn a_list_is_as_long_as_a_reply_may_be_and_says_so_when_it_is_longer() {
    let f = Fixture::new();
    // 400 chats of about 500 bytes: 200 KB, more than a plain reply and well within a deflated one.
    let chats: Vec<Value> = (0..400).map(|_| f.info(&new_uuid())).collect();
    f.says("list.json", &json!(chats));
    let plain = f.call("chats.list", json!({})).await;
    assert_eq!(code(&plain), "response_too_large", "{plain}");
    assert!(message(&plain).contains("too many chats"), "{plain}");
    assert!(!message(&plain).contains("events"), "{plain}");
    let deflating = f.call_deflating("chats.list", json!({})).await;
    assert_eq!(deflating["ok"], true, "{deflating}");
    assert_eq!(deflating["result"]["chats"].as_array().unwrap().len(), 400);
}

#[tokio::test]
async fn snapshots_validate_before_cli_and_preserve_complete_rows_with_bounded_argv() {
    let f = Fixture::without_cli();
    for extra in [
        json!({"limit":0}),
        json!({"limit":101}),
        json!({"cursor":"../escape"}),
        json!({"cursor":null}),
        json!({"before":-1}),
        json!({"before":2}),
        json!({"extra":true}),
        json!({"item_ids":["a"]}),
        json!({"item_ids":"bad"}),
    ] {
        let mut params = json!({"chat_id":f.chat});
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(
            code(&f.call("chat.snapshot", params).await),
            "invalid_request"
        );
    }
    let f = Fixture::new();
    let full = json!({"v":1,"chat_id":f.chat,"cursor":"1234-99-abcdef","next":99,
        "before":90,"more":true,"items":[{"order":90,"item":{"id":"row","status":"completed","body":{"type":"agent_message","text":"full base plus every delta"}}}],
        "controls":[{"event":"models","models":[]}]});
    f.says("snapshot.json", &full);
    let answer = f.call("chat.snapshot", json!({"chat_id":f.chat})).await;
    assert_eq!(answer["result"], full);
    let args = &f.chat_calls()[0];
    assert!(args.contains(&"snapshot".into()));
    assert!(args.contains(&"--max-bytes".into()));
    assert!(!args.contains(&"--since".into()));
    assert_eq!(
        f.calls_of("capabilities", "--json").len(),
        0,
        "read-only snapshot never asks a host about capabilities"
    );
    let mut older = full.clone();
    older["controls"] = json!([]);
    f.says("snapshot.json", &older);
    let answer = f
        .call(
            "chat.snapshot",
            json!({"chat_id":f.chat,"cursor":"1234-99-abcdef","before":91}),
        )
        .await;
    assert_eq!(answer["result"], older);
    f.says("snapshot.json", &full);
    assert_eq!(
        code(
            &f.call(
                "chat.snapshot",
                json!({"chat_id":f.chat,"cursor":"1234-99-abcdef","before":91})
            )
            .await
        ),
        "cli_error",
        "history cannot replay controls"
    );
    f.set(
        "snapshot.error",
        "Usage: riwork chat serve|ensure|list|new|events|command|send|stop (riwork help)",
    );
    let answer = f.call("chat.snapshot", json!({"chat_id":f.chat})).await;
    assert_eq!(code(&answer), "invalid_request");
    assert!(message(&answer).contains("unsupported RPC method"));
}

#[tokio::test]
async fn lossless_events_are_additive_and_do_not_truncate_or_skip_state() {
    let f = Fixture::new();
    let event = json!({"seq":1,"event":{"event":"item_completed","item":{"id":"big","status":"completed","body":{"type":"agent_message","text":noise(300_000,42)}}}});
    f.says("events.json", &f.page(&[event], 1, false));
    let response = f
        .call_deflating(
            "chat.events",
            json!({"chat_id":f.chat,"since":0,"wait_ms":0,"complete":true}),
        )
        .await;
    assert_eq!(code(&response), "response_too_large");
    assert!(f.calls_of("chat", "events")[0].contains(&"--complete".into()));
    let response = f
        .call_deflating(
            "chat.events",
            json!({"chat_id":f.chat,"since":0,"wait_ms":0}),
        )
        .await;
    assert_eq!(
        response["result"]["next"], 1,
        "legacy fitting stays compatible"
    );
    assert_eq!(
        code(
            &f.call_deflating(
                "chat.events",
                json!({"chat_id":f.chat,"since":0,"wait_ms":0,"complete":"true"})
            )
            .await
        ),
        "invalid_request"
    );
}

#[tokio::test]
async fn complete_pages_reject_missing_events_and_unrepresented_cursor_advances() {
    let f = Fixture::new();
    for mode in ["complete", "bounded"] {
        for (seqs, next) in [(vec![2], 2), (vec![1, 1], 1), (vec![1], 3), (vec![], 1)] {
            let events: Vec<_> = seqs
                .into_iter()
                .map(|seq| json!({"seq":seq,"event":{"event":"state","state":"idle"}}))
                .collect();
            f.says("events.json", &f.page(&events, next, false));
            let response = f
                .call(
                    "chat.events",
                    json!({"chat_id":f.chat,"since":0,"wait_ms":0,(mode):true}),
                )
                .await;
            assert_eq!(code(&response), "invalid_reply", "{response}");
        }
    }
}

#[tokio::test]
async fn bounded_recovery_keeps_identity_and_controls_and_refuses_unrepresented_state() {
    let f = Fixture::new();
    let body = json!({"seq":1,"event":{"event":"item_completed","item":{"id":"stable","turn_id":"turn","status":"completed","body":{"type":"agent_message","text":noise(300_000,42)}}}});
    f.says("events.json", &f.page(&[body], 1, true));
    let reply = f
        .call_deflating(
            "chat.events",
            json!({"chat_id":f.chat,"since":0,"wait_ms":0,"bounded":true}),
        )
        .await;
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"]["next"], 1);
    let item = &reply["result"]["events"][0]["event"]["item"];
    assert_eq!(item["id"], "stable");
    assert_eq!(item["turn_id"], "turn");
    assert_eq!(item["status"], "completed");
    assert!(item["body"]["text"].as_str().unwrap().ends_with('…'));
    assert!(f.calls_of("chat", "events")[0].contains(&"--bounded".into()));
    let control = json!({"seq":2,"event":{"event":"approval_requested","approval":{"request_id":"exact-request","choices":["accept","decline"],"title":noise(300_000,42)}}});
    f.says("events.json", &f.page(&[control.clone()], 2, false));
    let reply = f
        .call_deflating(
            "chat.events",
            json!({"chat_id":f.chat,"since":1,"wait_ms":0,"bounded":true}),
        )
        .await;
    assert_eq!(code(&reply), "response_too_large", "{reply}");
    f.says("events.json", &f.page(&[json!({"seq":2,"event":{"event":"approval_requested","approval":{"request_id":"exact-request","choices":["accept","decline"],"title":"complete control"}}})], 2, false));
    let reply = f
        .call(
            "chat.events",
            json!({"chat_id":f.chat,"since":1,"wait_ms":0,"bounded":true}),
        )
        .await;
    assert_eq!(
        reply["result"]["events"][0]["event"]["approval"]["title"],
        "complete control"
    );
    assert_eq!(
        code(
            &f.call(
                "chat.events",
                json!({"chat_id":f.chat,"since":0,"wait_ms":0,"complete":true,"bounded":true})
            )
            .await
        ),
        "invalid_request"
    );
}

#[tokio::test]
async fn bounded_placeholder_uses_product_copy_without_changing_cursor_or_identity() {
    let f = Fixture::new();
    let mut event = json!({"seq":1,"event":{"event":"item_completed","item":{"id":"large-structured-body","turn_id":"turn","status":"completed","body":{"type":"tool_call","tool":"fixture","input":[]}}}});
    let bare = f.page(&[event.clone()], 1, false).to_string().len();
    let count = (MAX_PLAINTEXT - 20 - bare) / 4;
    event["event"]["item"]["body"]["input"] = json!(vec!["x"; count]);
    f.says("events.json", &f.page(&[event], 1, false));
    let reply = f
        .call(
            "chat.events",
            json!({"chat_id":f.chat,"since":0,"wait_ms":0,"bounded":true}),
        )
        .await;
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["result"]["next"], 1);
    let item = &reply["result"]["events"][0]["event"]["item"];
    assert_eq!(item["id"], "large-structured-body");
    assert_eq!(item["turn_id"], "turn");
    assert_eq!(
        item["body"]["text"],
        "This message is too long to show here. Full text is on your Mac."
    );
}

#[tokio::test]
async fn a_switch_is_rebuilt_from_what_was_checked_and_needs_a_cli_that_has_it() {
    let f = Fixture::new();
    let switch = json!({"command":"switch","provider":"claude","model":"opus"});
    // A CLI that has chats but not the switch is not handed one.
    let response = f
        .call("chat.command", json!({"chat_id":f.chat,"command":switch}))
        .await;
    assert_eq!(code(&response), "cli_error", "{response}");
    assert!(
        message(&response).contains("another provider"),
        "{response}"
    );
    assert!(f.calls_of("chat", "command").is_empty());

    f.says(
        "capabilities.out",
        &json!({"v":1,"chat":true,"chat_provider_switch":true}),
    );
    let response = f
        .call("chat.command", json!({"chat_id":f.chat,"command":switch}))
        .await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        f.calls_of("chat", "command").pop().unwrap()[4],
        r#"{"command":"switch","model":"opus","provider":"claude"}"#
    );
    // Blank is left out, as in configure.
    f.call(
        "chat.command",
        json!({"chat_id":f.chat,"command":{"command":"switch","provider":"codex","effort":" ","fast":true}}),
    )
    .await;
    assert_eq!(
        f.calls_of("chat", "command").pop().unwrap()[4],
        r#"{"command":"switch","fast":true,"provider":"codex"}"#
    );
    let calls = f.calls_of("chat", "command").len();
    for bad in [
        json!({"command":"switch"}),
        json!({"command":"switch","provider":"grok"}),
        json!({"command":"switch","provider":null}),
        json!({"command":"switch","provider":"claude","approval_mode":"full"}),
        json!({"command":"switch","provider":"claude","fast":"yes"}),
        json!({"command":"switch","provider":"claude","model":"x".repeat(101)}),
    ] {
        let response = f
            .call("chat.command", json!({"chat_id":f.chat,"command":bad}))
            .await;
        assert_eq!(code(&response), "invalid_request", "{bad}: {response}");
    }
    assert_eq!(f.calls_of("chat", "command").len(), calls);
}

#[tokio::test]
async fn chat_models_passes_on_a_providers_saved_list_and_nothing_else() {
    let f = Fixture::new();
    let response = f.call("chat.models", json!({"provider":"claude"})).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    assert!(f.calls_of("chat", "models").is_empty());

    f.says(
        "capabilities.out",
        &json!({"v":1,"chat":true,"chat_models":true}),
    );
    f.says(
        "models.json",
        &json!({"provider":"claude","models":[{"id":"opus","name":"Opus","efforts":["high"],"extra":1}],
                "configured":["claude-opus-4"],"account_label":null,"error":null,"secret":"x"}),
    );
    let response = f
        .call(
            "chat.models",
            json!({"provider":"claude","project_id":f.project}),
        )
        .await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["result"],
        json!({"provider":"claude","models":[{"id":"opus","name":"Opus","efforts":["high"],"extra":1}],
               "configured":["claude-opus-4"],"account_label":null,"error":null})
    );
    let call = f.calls_of("chat", "models").pop().unwrap();
    assert_eq!(
        call[..6],
        [
            "chat",
            "models",
            "--provider",
            "claude",
            "--project",
            f.project.as_str()
        ]
    );

    // An answer for another provider, or one without names, does not reach the phone.
    f.says(
        "models.json",
        &json!({"provider":"codex","models":[],"configured":[]}),
    );
    let response = f.call("chat.models", json!({"provider":"claude"})).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    f.says(
        "models.json",
        &json!({"provider":"claude","models":[{"id":"opus"}],"configured":[]}),
    );
    let response = f.call("chat.models", json!({"provider":"claude"})).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    for bad in [
        json!({}),
        json!({"provider":"grok"}),
        json!({"provider":"claude","project_id":"nope"}),
        json!({"provider":"claude","x":1}),
    ] {
        let response = f.call("chat.models", bad.clone()).await;
        assert_eq!(code(&response), "invalid_request", "{bad}: {response}");
    }
}
