//! `project.create`: validation before any CLI runs, the argv it turns into (one argument per
//! value, never a shell string, and `--exclusive` only after the CLI said it knows the flag), what
//! it accepts back from the CLI, and its errors, against a stub CLI that records its argv and
//! prints whatever `create.json` holds. The one test against the real CLI is ignored and runs in a
//! throwaway RIWORK_HOME *and* HOME: it never touches a real installation or `~/Documents/riwork`.
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
        self.call(req("project.create", params)).await
    }
    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.stub.path().join(name), value).unwrap();
    }
    fn cli_says(&self, value: Value) {
        self.set("create.json", &value.to_string());
    }
    /// What `riwork capabilities --json` prints.
    fn capabilities(&self, value: &str) {
        self.set("capabilities.out", value);
    }
    /// The CLI fails `project create` with this one line on stderr.
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
    /// What the CLI prints for a project it just made, with the fields the phone must not see.
    fn project(&self, id: &str, name: &str) -> Value {
        json!({
            "id": id,
            "name": name,
            "root": format!("/Users/me/Documents/riwork/{name}"),
            "repository_roots": [format!("/Users/me/Documents/riwork/{name}")],
            "folder_id": "folder-secret",
            "notify_on_agent_done": true,
            "codex_account": {"source": "saved", "account_id": "acct-secret"},
            "created_at": 1790000000u64
        })
    }
}

/// Logs each call (arguments separated by U+001F). `capabilities` prints `capabilities.out` (by
/// default a CLI that knows `--exclusive`), or fails like a CLI from before the command if
/// `capabilities.unknown` exists. `project create` waits for `create.gate` (20 s at
/// most) if `create.hold` exists, marks `create.ran`, then prints `create.json`, or fails with
/// the line in `create.error`.
/// `project list` prints `projects.json` (default `[]`).
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
               if [ -e \"$d/capabilities.out\" ]; then cat \"$d/capabilities.out\"; else printf '{{\"v\":1,\"verifies_shell\":true,\"project_create_exclusive\":true}}'; fi;;\n\
             'project create')\n\
               if [ -e \"$d/create.hold\" ]; then i=0; while [ ! -e \"$d/create.gate\" ] && [ $i -lt 400 ]; do sleep 0.05; i=$((i+1)); done; fi\n\
               touch \"$d/create.ran\"\n\
               if [ -e \"$d/create.error\" ]; then printf 'riwork: %s\\n' \"$(cat \"$d/create.error\")\" >&2; exit 2; fi\n\
               cat \"$d/create.json\";;\n\
             'project list') if [ -e \"$d/projects.json\" ]; then cat \"$d/projects.json\"; else echo '[]'; fi;;\n\
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
    let long = "x".repeat(101);
    // 86 three-byte characters are 258 bytes: few characters, too many bytes.
    let heavy = "\u{65e5}".repeat(86);
    // 64 four-byte characters are 256 bytes.
    let emoji = "\u{1f600}".repeat(64);
    assert!(heavy.chars().count() <= 100 && heavy.len() > 255);
    assert!(emoji.chars().count() <= 100 && emoji.len() > 255);
    let bad = [
        // Not an object.
        json!(null),
        json!([]),
        json!("Fresh"),
        json!(7),
        json!({}),
        // The name: required, a string.
        json!({"git":true}),
        json!({"name":null}),
        json!({"name":7}),
        json!({"name":true}),
        json!({"name":["Fresh"]}),
        json!({"name":{"name":"Fresh"}}),
        // git: a boolean, never null.
        json!({"name":"Fresh","git":null}),
        json!({"name":"Fresh","git":"true"}),
        json!({"name":"Fresh","git":"false"}),
        json!({"name":"Fresh","git":1}),
        json!({"name":"Fresh","git":0}),
        json!({"name":"Fresh","git":[]}),
        // Nothing else is a parameter: the phone never says where, the desktop decides.
        json!({"name":"Fresh","path":"/tmp/x"}),
        json!({"name":"Fresh","root":"/tmp/x"}),
        json!({"name":"Fresh","location":"/tmp"}),
        json!({"name":"Fresh","directory":"~"}),
        json!({"name":"Fresh","cwd":"/tmp"}),
        json!({"name":"Fresh","parent":"/tmp"}),
        json!({"name":"Fresh","folder_id":new_uuid()}),
        json!({"name":"Fresh","template":"rust"}),
        json!({"name":"Fresh","no_git":true}),
        json!({"name":"Fresh","exclusive":false}),
        json!({"name":"Fresh","json":true}),
        json!({"name":"Fresh","owner":f.device}),
        json!({"name":"Fresh","Name":"Other"}),
        json!({"Name":"Fresh"}),
        // Empty, or whitespace at either end (the CLI trims, so it would not be the name checked).
        json!({"name":""}),
        json!({"name":" "}),
        json!({"name":"\t"}),
        json!({"name":" Fresh"}),
        json!({"name":"Fresh "}),
        json!({"name":"\u{a0}Fresh"}),
        json!({"name":"Fresh\u{a0}"}),
        json!({"name":"\u{3000}Fresh"}),
        json!({"name":"Fresh\u{2003}"}),
        // Length, in characters and in bytes.
        json!({"name":long}),
        json!({"name":heavy}),
        json!({"name":emoji}),
        // One folder name: no separators, nothing hidden, and not `.` or `..`.
        json!({"name":"."}),
        json!({"name":".."}),
        json!({"name":"..."}),
        json!({"name":".git"}),
        json!({"name":".GIT"}),
        json!({"name":".hidden"}),
        json!({"name":".ssh"}),
        json!({"name":"a/b"}),
        json!({"name":"/etc"}),
        json!({"name":"../x"}),
        json!({"name":"x/.."}),
        json!({"name":"~/x"}),
        json!({"name":"a\\b"}),
        json!({"name":"\\x"}),
        // No control characters of any kind.
        json!({"name":"a\u{0}b"}),
        json!({"name":"a\tb"}),
        json!({"name":"a\nb"}),
        json!({"name":"a\rb"}),
        json!({"name":"a\u{1b}[2Jb"}),
        json!({"name":"a\u{7f}b"}),
        json!({"name":"a\u{85}b"}),
        json!({"name":"a\u{9b}b"}),
        json!({"name":"a\u{2028}b"}),
        json!({"name":"a\u{2029}b"}),
        // The CLI would read these as options of its own.
        json!({"name":"-"}),
        json!({"name":"-x"}),
        json!({"name":"--json"}),
        json!({"name":"--name"}),
        json!({"name":"--no-git"}),
        json!({"name":"--exclusive"}),
        json!({"name":"--"}),
        json!({"name":"-h"}),
    ];
    for params in bad {
        let response = f.create(params.clone()).await;
        assert_eq!(code(&response), "invalid_request", "{params}: {response}");
    }
}

#[tokio::test]
async fn every_value_is_its_own_argument_and_the_cli_gets_json_last() {
    let f = Fixture::new();
    // Everything a shell would act on, except a slash: that is refused (a name is one folder).
    let hostile = "echo \"hi\" ; $(touch x) `id` | cat > null && exit 'a b' $HOME \u{e9}\u{1f600}";
    for (params, name, git) in [
        (json!({"name":"Fresh"}), "Fresh", true),
        (json!({"name":"Fresh","git":true}), "Fresh", true),
        (json!({"name":"Plain","git":false}), "Plain", false),
        (json!({"name":hostile}), hostile, true),
        (json!({"name":hostile,"git":false}), hostile, false),
        (json!({"name":"my--app"}), "my--app", true),
        (json!({"name":"a b"}), "a b", true),
    ] {
        f.cli_says(f.project(&new_uuid(), name));
        let before = f.calls().len();
        let response = f.create(params.clone()).await;
        assert_eq!(response["ok"], true, "{params}: {response}");
        let calls = f.calls();
        // The CLI is asked what it knows, then makes the project: nothing else runs.
        assert_eq!(calls.len(), before + 2, "{params}: {calls:?}");
        assert_eq!(
            calls[calls.len() - 2],
            vec!["capabilities".to_owned(), "--json".into()]
        );
        let mut expected: Vec<String> = ["project", "create", "--name", name]
            .map(String::from)
            .to_vec();
        if !git {
            expected.push("--no-git".into());
        }
        expected.push("--exclusive".into());
        expected.push("--json".into());
        assert_eq!(calls.last().unwrap(), &expected, "{params}");
    }
    assert_eq!(f.calls_of("project", "create").len(), 7);
}

#[tokio::test]
async fn the_result_is_the_new_project_as_the_list_shows_it_without_private_fields() {
    let f = Fixture::new();
    let id = new_uuid();
    let created = f.project(&id, "Fresh");
    f.cli_says(created.clone());
    let response = f.create(json!({"name":"Fresh"})).await;
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["result"],
        json!({
            "project_id": id,
            "project": {
                "id": id,
                "name": "Fresh",
                "root": "/Users/me/Documents/riwork/Fresh",
                "created_at": 1790000000u64
            }
        })
    );
    let text = response.to_string();
    for private in [
        "repository_roots",
        "folder-secret",
        "folder_id",
        "notify_on_agent_done",
        "codex_account",
        "acct-secret",
    ] {
        assert!(!text.contains(private), "{private} leaked: {text}");
    }
    // The same projection `projects.list` uses, field for field.
    f.set("projects.json", &json!([created]).to_string());
    let listed = f.call(req("projects.list", json!({}))).await;
    assert_eq!(
        listed["result"]["projects"],
        json!([response["result"]["project"].clone()])
    );
}

#[tokio::test]
async fn a_project_that_was_not_asked_for_is_refused() {
    let f = Fixture::new();
    let id = new_uuid();
    let good = f.project(&id, "Fresh");
    let with = |field: &str, value: Value| {
        let mut project = good.clone();
        project[field] = value;
        project
    };
    let without = |field: &str| {
        let mut project = good.clone();
        project.as_object_mut().unwrap().remove(field);
        project
    };
    let wrong = [
        with("id", json!("not-a-uuid")),
        with("id", json!(id.to_uppercase())),
        with("id", json!(id.replace('-', ""))),
        with("id", json!(7)),
        without("id"),
        // Another name than the one sent.
        with("name", json!("Other")),
        with("name", json!("fresh")),
        with("name", json!("Fresh ")),
        without("name"),
        // Not the default location of that name.
        with("root", json!("Fresh")),
        with("root", json!("Documents/riwork/Fresh")),
        with("root", json!("/Users/me/Documents/riwork/Other")),
        with("root", json!("/Users/me/Documents/riwork/Fresh/sub")),
        with("root", json!("/")),
        with("root", json!(7)),
        without("root"),
        with("created_at", json!("1790000000")),
        with("created_at", json!(-1)),
        with("created_at", json!(1.5)),
        without("created_at"),
        // Not a project.
        json!([good.clone()]),
        json!("created"),
        json!(null),
        json!({}),
    ];
    for project in wrong {
        f.cli_says(project.clone());
        let response = f.create(json!({"name":"Fresh"})).await;
        assert_eq!(code(&response), "cli_error", "{project}: {response}");
        assert_eq!(
            message(&response),
            "CLI returned a project that does not match the request",
            "{project}"
        );
    }
}

#[tokio::test]
async fn a_cli_that_does_not_know_the_flag_is_never_sent_it() {
    let f = Fixture::new();
    f.cli_says(f.project(&new_uuid(), "Fresh"));
    let sentence = "the installed riwork CLI cannot create projects from the phone; update RiWork";
    for (what, apply) in [
        (
            "from before the command",
            Box::new(|f: &Fixture| f.set("capabilities.unknown", "")) as Box<dyn Fn(&Fixture)>,
        ),
        (
            "an answer without the flag",
            Box::new(|f: &Fixture| {
                let _ = std::fs::remove_file(f.stub.path().join("capabilities.unknown"));
                f.capabilities("{\"v\":1,\"verifies_shell\":true}");
            }),
        ),
        (
            "an answer that says no",
            Box::new(|f: &Fixture| f.capabilities("{\"v\":1,\"project_create_exclusive\":false}")),
        ),
        (
            "yes in the wrong type",
            Box::new(|f: &Fixture| {
                f.capabilities("{\"v\":1,\"project_create_exclusive\":\"true\"}")
            }),
        ),
        (
            "yes in another version",
            Box::new(|f: &Fixture| f.capabilities("{\"v\":2,\"project_create_exclusive\":true}")),
        ),
        (
            "something that is not JSON",
            Box::new(|f: &Fixture| f.capabilities("project_create_exclusive yes")),
        ),
        ("nothing at all", Box::new(|f: &Fixture| f.capabilities(""))),
    ] {
        apply(&f);
        let response = f.create(json!({"name":"Fresh"})).await;
        assert_eq!(code(&response), "cli_error", "{what}: {response}");
        assert_eq!(message(&response), sentence, "{what}");
    }
    // Not once was `project create` run, with a flag it would read as a PATH.
    assert!(
        f.calls_of("project", "create").is_empty(),
        "{:?}",
        f.calls()
    );
    // A CLI that says yes is believed at once, even after having said no.
    f.capabilities("{\"v\":1,\"project_create_exclusive\":true}");
    let response = f.create(json!({"name":"Fresh"})).await;
    assert_eq!(response["ok"], true, "{response}");
    // And one that cannot be run at all is an error, not a creation.
    let gone = Fixture::without_cli();
    let response = gone.create(json!({"name":"Fresh"})).await;
    assert_eq!(code(&response), "cli_error", "{response}");
}

#[tokio::test]
async fn a_name_or_folder_that_is_taken_has_its_own_code_and_no_path() {
    let f = Fixture::new();
    let params = json!({"name":"Fresh"});
    let cases = [
        (
            "already_exists: project Fresh already exists (/Users/me/code/Fresh)",
            "already_exists",
            "A project named \"Fresh\" already exists on the desktop",
        ),
        (
            "already_exists: folder /Users/me/Documents/riwork/Fresh already exists",
            "already_exists",
            "A folder named \"Fresh\" already exists in the desktop's projects folder",
        ),
        (
            "already_exists: folder /Users/me/Documents/riwork/Fresh is already a project",
            "already_exists",
            "A folder named \"Fresh\" already exists in the desktop's projects folder",
        ),
        // Anything else is the CLI's own words, without the wrapper.
        (
            "git init failed: fatal: cannot mkdir",
            "cli_error",
            "git init failed: fatal: cannot mkdir",
        ),
        (
            "Cannot create /Users/me/Documents/riwork/Fresh: Permission denied (os error 13)",
            "cli_error",
            "Cannot create /Users/me/Documents/riwork/Fresh: Permission denied (os error 13)",
        ),
        (
            "Cannot launch git: No such file or directory (os error 2)",
            "cli_error",
            "Cannot launch git: No such file or directory (os error 2)",
        ),
        // A look-alike is not the token.
        (
            "an already_exists: project in the middle",
            "cli_error",
            "an already_exists: project in the middle",
        ),
        (
            "already_exists:project Fresh",
            "cli_error",
            "already_exists:project Fresh",
        ),
        (
            "already_exists: projects Fresh",
            "cli_error",
            "already_exists: projects Fresh",
        ),
        (
            "Already_exists: project Fresh",
            "cli_error",
            "Already_exists: project Fresh",
        ),
    ];
    for (line, expected, text) in cases {
        f.cli_fails(line);
        let response = f.create(params.clone()).await;
        assert_eq!(code(&response), expected, "{line}: {response}");
        assert_eq!(message(&response), text, "{line}");
    }
    // The sentence never carries a path of the desktop.
    f.cli_fails("already_exists: folder /Users/me/Documents/riwork/Fresh already exists");
    let response = f.create(params.clone()).await;
    assert!(!message(&response).contains("/Users"), "{response}");
    // Only the first line of the CLI's error decides.
    f.cli_fails("tmux: boom\nalready_exists: project Fresh already exists");
    let response = f.create(params.clone()).await;
    assert_eq!(code(&response), "cli_error", "{response}");
    assert_eq!(message(&response), "tmux: boom");
    // The name in the sentence is the one that was sent, in the case it was sent.
    f.cli_fails("already_exists: project FRESH already exists (/x)");
    let response = f.create(json!({"name":"fresh"})).await;
    assert_eq!(
        message(&response),
        "A project named \"fresh\" already exists on the desktop"
    );
}

#[tokio::test]
async fn a_project_being_made_is_finished_when_the_request_is_dropped() {
    // The connector drops a request's task when its connection ends (relay error, revocation):
    // the CLI must not be killed between making the folder and writing the project down.
    let f = std::sync::Arc::new(Fixture::new());
    f.cli_says(f.project(&new_uuid(), "Fresh"));
    f.set("create.hold", "");
    let task = {
        let f = f.clone();
        tokio::spawn(async move { f.create(json!({"name":"Fresh"})).await })
    };
    // Wait until the CLI is running, then drop the request.
    for _ in 0..500 {
        if !f.calls_of("project", "create").is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(f.calls_of("project", "create").len(), 1);
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
async fn creation_is_not_deduplicated_a_second_request_runs_the_cli_again() {
    let f = Fixture::new();
    f.cli_says(f.project(&new_uuid(), "Fresh"));
    let first = f.create(json!({"name":"Fresh"})).await;
    assert_eq!(first["ok"], true, "{first}");
    // The desktop's answer to a repeat is the CLI's: here it is taken, as it would be.
    f.cli_fails("already_exists: project Fresh already exists (/x)");
    let again = f.create(json!({"name":"Fresh"})).await;
    assert_eq!(code(&again), "already_exists", "{again}");
    assert_eq!(f.calls_of("project", "create").len(), 2);
    // Nothing about it is recorded as input: the request id is free to use again.
    let reused = req("project.create", json!({"name":"Fresh"}));
    f.cli_fails("already_exists: project Fresh already exists (/x)");
    for _ in 0..2 {
        let response = f.call(reused.clone()).await;
        assert_eq!(code(&response), "already_exists", "{response}");
    }
}

/// The real CLI, in a throwaway RIWORK_HOME and a throwaway HOME (so `~/Documents/riwork` is the
/// test's own), behind a wrapper.
struct RealCli {
    dir: tempfile::TempDir,
    wrapper: PathBuf,
}
impl RealCli {
    fn new(cli: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // macOS temp dirs are symlinks; project roots are stored canonically.
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
    fn home(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap().join("home")
    }
}

#[tokio::test]
#[ignore = "requires RIWORK_TEST_CLI; real CLI in a throwaway HOME and RIWORK_HOME, makes only its own folders"]
async fn the_real_cli_creates_a_project_in_its_own_default_folder_through_the_rpc() {
    let cli = PathBuf::from(std::env::var_os("RIWORK_TEST_CLI").expect("set RIWORK_TEST_CLI"));
    assert!(cli.is_absolute(), "absolute CLI path required");
    let real = RealCli::new(&cli);
    let projects = real.home().join("Documents/riwork");
    assert!(!projects.exists());

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

    // A repository by default.
    let git = call("project.create", json!({"name":"Phone App"})).await;
    assert_eq!(git["ok"], true, "{git}");
    let root = projects.join("Phone App");
    assert_eq!(git["result"]["project"]["root"], root.to_str().unwrap());
    assert_eq!(git["result"]["project"]["name"], "Phone App");
    assert!(root.join(".git").is_dir());
    // A plain folder when asked.
    let plain = call("project.create", json!({"name":"Plain","git":false})).await;
    assert_eq!(plain["ok"], true, "{plain}");
    assert!(projects.join("Plain").is_dir() && !projects.join("Plain/.git").exists());

    // Both are in the list exactly as the create answers gave them, and nothing else is.
    let listed = call("projects.list", json!({})).await;
    let listed = listed["result"]["projects"].as_array().unwrap().clone();
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert!(listed.contains(&git["result"]["project"]), "{listed:?}");
    assert!(listed.contains(&plain["result"]["project"]), "{listed:?}");
    assert_eq!(
        git["result"]["project_id"], git["result"]["project"]["id"],
        "{git}"
    );

    // A taken name, in any case, and a folder that is there: refused, and nothing touched.
    for name in ["Phone App", "phone app", "PHONE APP"] {
        let again = call("project.create", json!({ "name": name })).await;
        assert_eq!(code(&again), "already_exists", "{name}: {again}");
        assert!(
            message(&again).contains("project"),
            "{name}: {}",
            message(&again)
        );
        assert!(!message(&again).contains(real.home().to_str().unwrap()));
    }
    std::fs::create_dir_all(projects.join("Mine")).unwrap();
    std::fs::write(projects.join("Mine/notes.txt"), "keep").unwrap();
    let mine = call("project.create", json!({"name":"Mine"})).await;
    assert_eq!(code(&mine), "already_exists", "{mine}");
    assert!(message(&mine).contains("folder"), "{}", message(&mine));
    assert_eq!(
        std::fs::read_to_string(projects.join("Mine/notes.txt")).unwrap(),
        "keep"
    );
    assert!(!projects.join("Mine/.git").exists());
    let after = call("projects.list", json!({})).await;
    assert_eq!(after["result"]["projects"].as_array().unwrap().len(), 2);

    // Validation never reached the CLI: nothing was made for these.
    for name in ["../escape", ".hidden", "--json", " padded "] {
        let bad = call("project.create", json!({ "name": name })).await;
        assert_eq!(code(&bad), "invalid_request", "{name}: {bad}");
    }
    let entries: Vec<_> = std::fs::read_dir(&projects)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries.len(), 3, "{entries:?}");
    assert!(!real.home().join("escape").exists());
}
