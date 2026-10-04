//! Files from the phone: `upload.begin|chunk|finish|cancel` and `shell.paste`, through the RPC
//! layer against a stub CLI that records its argv. The files land in a throwaway RIWORK_HOME.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use riwork_remote::{
    config::Storage,
    rpc::Rpc,
    upload::{self, CHUNK_BYTES, KEEP_SECONDS, MAX_ACTIVE, MAX_FILE_BYTES, Uploads},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn req(method: &str, params: Value) -> Value {
    json!({"v":1,"type":"request","id":new_uuid(),"method":method,"params":params})
}
fn sha(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}
fn pair(storage: &Storage, dir: &Path, name: &str) -> String {
    storage
        .pair(
            "wss://example.com/v1/ws".into(),
            name.into(),
            false,
            &dir.join(format!("{name}.json")),
            None,
        )
        .unwrap()
        .device_id
}
fn code(response: &Value) -> &str {
    response["error"]["code"].as_str().unwrap_or("")
}
fn result(response: &Value) -> &Value {
    assert_eq!(response["ok"], true, "{response}");
    &response["result"]
}
fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// A RIWORK_HOME with its `remote` storage, a paired phone, a live shell and a stub CLI.
struct Fixture {
    home: tempfile::TempDir,
    stub: tempfile::TempDir,
    rpc: Rpc,
    device: String,
    shell: String,
}
impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let stub = tempfile::tempdir().unwrap();
        let storage = Storage::at(home.path().into()).unwrap();
        let device = pair(&storage, home.path(), "phone");
        let shell = new_uuid();
        let cli = stub_cli(stub.path(), &shell);
        Self {
            rpc: Rpc::new(cli, storage),
            home,
            stub,
            device,
            shell,
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
        self.call_as(&self.device, method, params).await
    }
    async fn call_as(&self, device: &str, method: &str, params: Value) -> Value {
        self.rpc.handle(device, req(method, params)).await.unwrap()
    }
    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.stub.path().join(name), value).unwrap();
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
    fn paste_calls(&self) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with(&["shell".to_owned(), "paste".to_owned()]))
            .collect()
    }
    fn begin_params(&self, upload: &str, name: &str, data: &[u8]) -> Value {
        json!({"upload":upload,"shell_id":self.shell,"name":name,"size":data.len(),"type":"image/png","sha256":sha(data)})
    }
    /// Begin, send in chunks and finish; the finished file's path.
    async fn send(&self, name: &str, data: &[u8]) -> (String, PathBuf) {
        let upload = new_uuid();
        let begun = self
            .call("upload.begin", self.begin_params(&upload, name, data))
            .await;
        assert_eq!(result(&begun)["received"], 0);
        assert_eq!(result(&begun)["chunk_bytes"], CHUNK_BYTES);
        for (index, chunk) in data.chunks(CHUNK_BYTES).enumerate() {
            let answer = self
                .call(
                    "upload.chunk",
                    json!({"upload":upload,"offset":index*CHUNK_BYTES,"data":URL_SAFE_NO_PAD.encode(chunk)}),
                )
                .await;
            assert_eq!(result(&answer)["status"], "partial");
        }
        let finished = self.call("upload.finish", json!({"upload":upload})).await;
        let finished = result(&finished);
        assert_eq!(finished["status"], "complete");
        assert_eq!(finished["received"], data.len());
        (upload, PathBuf::from(finished["path"].as_str().unwrap()))
    }
    fn inbox(&self) -> PathBuf {
        self.home.path().join("uploads").join(&self.shell)
    }
}

/// Logs each call (arguments separated by U+001F), reports one live shell until `dead` exists,
/// answers `capabilities` with `shell_paste` unless `old` exists, and answers `shell paste` as
/// `mode` says.
fn stub_cli(dir: &Path, shell: &str) -> PathBuf {
    let cli = dir.join("fake-riwork");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\n\
             d='{dir}'\n\
             for a in \"$@\"; do printf '%s\\037' \"$a\"; done >> \"$d/argv.log\"\n\
             printf '\\n' >> \"$d/argv.log\"\n\
             case \"$1 $2\" in\n\
             'capabilities --json') if [ -e \"$d/old\" ]; then echo '{{\"v\":1,\"chat\":true}}'; else echo '{{\"v\":1,\"chat\":true,\"shell_paste\":true}}'; fi;;\n\
             'shell list') if [ -e \"$d/dead\" ]; then echo '[]'; else printf '[{{\"id\":\"{shell}\",\"alive\":true}}]'; fi;;\n\
             'orchestrator list') echo '[]';;\n\
             'shell paste')\n\
               case \"$(cat \"$d/mode\" 2>/dev/null)\" in\n\
                 input_unavailable) echo 'riwork: input_unavailable: terminal input is disabled for this pane' >&2; exit 2;;\n\
                 not_sent) echo 'riwork: not_sent: tmux did not answer' >&2; exit 2;;\n\
                 old) printf \"riwork: Unknown shell command 'paste'\\nUsage...\\n\" >&2; exit 2;;\n\
                 partial) echo 'riwork: paste-buffer failed' >&2; exit 2;;\n\
               esac;;\n\
             esac\n",
            dir = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    cli
}

#[tokio::test]
async fn every_malformed_request_fails_before_anything_runs_or_is_written() {
    let f = Fixture::without_cli();
    let (upload, shell, chat) = (new_uuid(), new_uuid(), new_uuid());
    let good_sha = sha(b"x");
    let begin = |extra: Value| {
        let mut base =
            json!({"upload":upload,"shell_id":shell,"name":"a.png","size":1,"sha256":good_sha});
        for (k, v) in extra.as_object().unwrap() {
            if v.is_null() {
                base.as_object_mut().unwrap().remove(k);
            } else {
                base[k] = v.clone();
            }
        }
        base
    };
    let bad: Vec<(&str, &str, Value)> = vec![
        ("no params", "upload.begin", json!({})),
        (
            "unknown field",
            "upload.begin",
            begin(json!({"path":"/etc/passwd"})),
        ),
        (
            "short upload",
            "upload.begin",
            begin(json!({"upload":"1234"})),
        ),
        (
            "upper shell",
            "upload.begin",
            begin(json!({"shell_id":shell.to_uppercase()})),
        ),
        (
            "both targets",
            "upload.begin",
            begin(json!({"chat_id":chat})),
        ),
        ("no target", "upload.begin", begin(json!({"shell_id":null}))),
        (
            "traversal target",
            "upload.begin",
            begin(json!({"shell_id":"../../etc"})),
        ),
        ("empty name", "upload.begin", begin(json!({"name":""}))),
        (
            "control in name",
            "upload.begin",
            begin(json!({"name":"a\nb.png"})),
        ),
        (
            "long name",
            "upload.begin",
            begin(json!({"name":"a".repeat(256)})),
        ),
        (
            "bad type",
            "upload.begin",
            begin(json!({"type":"image/png; x=1"})),
        ),
        ("short sha", "upload.begin", begin(json!({"sha256":"abc"}))),
        ("negative size", "upload.begin", begin(json!({"size":-1}))),
        ("string size", "upload.begin", begin(json!({"size":"1"}))),
        (
            "chunk no data",
            "upload.chunk",
            json!({"upload":upload,"offset":0}),
        ),
        (
            "chunk empty",
            "upload.chunk",
            json!({"upload":upload,"offset":0,"data":""}),
        ),
        (
            "chunk padded",
            "upload.chunk",
            json!({"upload":upload,"offset":0,"data":"eA=="}),
        ),
        (
            "chunk not base64",
            "upload.chunk",
            json!({"upload":upload,"offset":0,"data":"*"}),
        ),
        (
            "chunk too big",
            "upload.chunk",
            json!({"upload":upload,"offset":0,"data":URL_SAFE_NO_PAD.encode(vec![0u8; CHUNK_BYTES + 1])}),
        ),
        (
            "chunk negative offset",
            "upload.chunk",
            json!({"upload":upload,"offset":-1,"data":"eA"}),
        ),
        (
            "finish extra",
            "upload.finish",
            json!({"upload":upload,"x":1}),
        ),
        ("cancel bad id", "upload.cancel", json!({"upload":"x"})),
        (
            "paste none",
            "shell.paste",
            json!({"shell_id":shell,"batch":upload,"uploads":[]}),
        ),
        (
            "paste many",
            "shell.paste",
            json!({"shell_id":shell,"batch":upload,"uploads":(0..17).map(|_| new_uuid()).collect::<Vec<_>>()}),
        ),
        (
            "paste twice",
            "shell.paste",
            json!({"shell_id":shell,"batch":upload,"uploads":[chat,chat]}),
        ),
        (
            "paste path",
            "shell.paste",
            json!({"shell_id":shell,"batch":upload,"uploads":["/etc/passwd"]}),
        ),
        (
            "paste no batch",
            "shell.paste",
            json!({"shell_id":shell,"uploads":[chat]}),
        ),
    ];
    for (name, method, params) in bad {
        let answer = f.call(method, params).await;
        assert_eq!(code(&answer), "invalid_request", "{name}: {answer}");
    }
    // Nothing was staged, no inbox was made and no ledger written.
    assert!(!f.home.path().join("uploads").exists());
    assert!(!f.home.path().join("remote/uploads").exists());
    assert!(
        !f.rpc
            .storage
            .dir
            .join(format!("uploads-{}.json", f.device))
            .exists()
    );
}

#[tokio::test]
async fn a_file_arrives_whole_private_and_under_a_name_of_the_desktops_making() {
    let f = Fixture::new();
    let data: Vec<u8> = (0..CHUNK_BYTES * 2 + 123)
        .map(|i| (i % 251) as u8)
        .collect();
    let (_, path) = f
        .send("../../.ssh/authorized_keys photo (1).PNG", &data)
        .await;
    assert_eq!(path.parent().unwrap(), f.inbox());
    let name = path.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("authorized_keys-photo-1-"), "{name}");
    assert!(name.ends_with(".png"), "{name}");
    assert_eq!(std::fs::read(&path).unwrap(), data);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&f.inbox()), 0o700);
    assert_eq!(mode(&f.home.path().join("uploads")), 0o700);
    // Nothing is left in staging, and nothing was run but the capability and shell checks.
    let staged = f.home.path().join("remote/uploads").join(&f.device);
    assert_eq!(std::fs::read_dir(staged).unwrap().count(), 0);
    assert!(f.paste_calls().is_empty());
    // The same name again is another file.
    let (_, again) = f
        .send("../../.ssh/authorized_keys photo (1).PNG", b"two")
        .await;
    assert_ne!(again, path);
    assert_eq!(std::fs::read(&path).unwrap(), data);
}

#[tokio::test]
async fn an_interrupted_upload_resumes_and_a_repeated_chunk_changes_nothing() {
    let f = Fixture::new();
    let data: Vec<u8> = (0..CHUNK_BYTES + 10).map(|i| (i % 7) as u8).collect();
    let upload = new_uuid();
    let params = f.begin_params(&upload, "a.bin", &data);
    result(&f.call("upload.begin", params.clone()).await);
    let first = URL_SAFE_NO_PAD.encode(&data[..CHUNK_BYTES]);
    let answer = f
        .call(
            "upload.chunk",
            json!({"upload":upload,"offset":0,"data":first}),
        )
        .await;
    assert_eq!(result(&answer)["received"], CHUNK_BYTES);
    // The answer was lost and the chunk comes again: nothing is added.
    let again = f
        .call(
            "upload.chunk",
            json!({"upload":upload,"offset":0,"data":first}),
        )
        .await;
    assert_eq!(result(&again)["received"], CHUNK_BYTES);
    // A new connection asks where it stands.
    let resumed = f.call("upload.begin", params.clone()).await;
    assert_eq!(result(&resumed)["status"], "partial");
    assert_eq!(result(&resumed)["received"], CHUNK_BYTES);
    // A chunk past what arrived, or past the size, is refused.
    let gap = f
        .call(
            "upload.chunk",
            json!({"upload":upload,"offset":CHUNK_BYTES + 1,"data":"eA"}),
        )
        .await;
    assert_eq!(code(&gap), "invalid_request", "{gap}");
    let past = f
        .call(
            "upload.chunk",
            json!({"upload":upload,"offset":CHUNK_BYTES,"data":URL_SAFE_NO_PAD.encode([0u8; 11])}),
        )
        .await;
    assert_eq!(code(&past), "invalid_request", "{past}");
    // Not all there: no finishing.
    let early = f.call("upload.finish", json!({"upload":upload})).await;
    assert_eq!(code(&early), "invalid_request", "{early}");
    let rest = URL_SAFE_NO_PAD.encode(&data[CHUNK_BYTES..]);
    result(
        &f.call(
            "upload.chunk",
            json!({"upload":upload,"offset":CHUNK_BYTES,"data":rest}),
        )
        .await,
    );
    let done = f.call("upload.finish", json!({"upload":upload})).await;
    let path = result(&done)["path"].as_str().unwrap().to_owned();
    assert_eq!(std::fs::read(&path).unwrap(), data);
    // Finishing again, beginning again and a late chunk all answer complete, with the same file.
    for answer in [
        f.call("upload.finish", json!({"upload":upload})).await,
        f.call("upload.begin", params.clone()).await,
        f.call(
            "upload.chunk",
            json!({"upload":upload,"offset":0,"data":first}),
        )
        .await,
    ] {
        assert_eq!(result(&answer)["status"], "complete", "{answer}");
        assert_eq!(result(&answer)["path"], path.as_str());
    }
    // The same UUID for another file is refused.
    let other = f
        .call("upload.begin", f.begin_params(&upload, "a.bin", b"other"))
        .await;
    assert_eq!(code(&other), "invalid_request", "{other}");
}

#[tokio::test]
async fn a_damaged_file_is_thrown_away_and_a_cancelled_one_too() {
    let f = Fixture::new();
    let upload = new_uuid();
    let mut params = f.begin_params(&upload, "a.png", b"hello");
    params["sha256"] = json!(sha(b"world"));
    result(&f.call("upload.begin", params).await);
    result(
        &f.call(
            "upload.chunk",
            json!({"upload":upload,"offset":0,"data":URL_SAFE_NO_PAD.encode(b"hello")}),
        )
        .await,
    );
    let damaged = f.call("upload.finish", json!({"upload":upload})).await;
    assert_eq!(code(&damaged), "invalid_request", "{damaged}");
    assert!(
        damaged["error"]["message"]
            .as_str()
            .unwrap()
            .contains("damaged")
    );
    assert!(!f.inbox().exists() || std::fs::read_dir(f.inbox()).unwrap().count() == 0);
    let gone = f.call("upload.finish", json!({"upload":upload})).await;
    assert_eq!(code(&gone), "not_found", "{gone}");

    let upload = new_uuid();
    result(
        &f.call("upload.begin", f.begin_params(&upload, "b.png", b"abc"))
            .await,
    );
    let cancelled = f.call("upload.cancel", json!({"upload":upload})).await;
    assert_eq!(result(&cancelled)["status"], "cancelled");
    let late = f
        .call(
            "upload.chunk",
            json!({"upload":upload,"offset":0,"data":"YWJj"}),
        )
        .await;
    assert_eq!(code(&late), "not_found", "{late}");
    // Cancelling what is unknown is fine.
    let unknown = f.call("upload.cancel", json!({"upload":new_uuid()})).await;
    assert_eq!(result(&unknown)["status"], "cancelled");
}

#[tokio::test]
async fn the_desktop_holds_the_limits() {
    let f = Fixture::new();
    let big = json!({"upload":new_uuid(),"shell_id":f.shell,"name":"big.mov","size":MAX_FILE_BYTES + 1,"sha256":sha(b"")});
    let answer = f.call("upload.begin", big).await;
    assert_eq!(code(&answer), "upload_limit", "{answer}");
    let empty =
        json!({"upload":new_uuid(),"shell_id":f.shell,"name":"e","size":0,"sha256":sha(b"")});
    assert_eq!(code(&f.call("upload.begin", empty).await), "upload_limit");
    // A small finished file, then partial ones up to the quota: the finished one makes room.
    let (_, small) = f.send("small.png", b"tiny").await;
    let large = |upload: &str| json!({"upload":upload,"shell_id":f.shell,"name":"l.mov","size":MAX_FILE_BYTES,"sha256":sha(b"")});
    for _ in 0..MAX_ACTIVE {
        let answer = f.call("upload.begin", large(&new_uuid())).await;
        assert_eq!(result(&answer)["status"], "partial", "{answer}");
    }
    assert!(!small.exists(), "the oldest finished upload makes room");
    // No more at once, and partial ones are never evicted.
    let answer = f.call("upload.begin", large(&new_uuid())).await;
    assert_eq!(code(&answer), "upload_limit", "{answer}");
    // Another phone has its own quota.
    let other = pair(&f.rpc.storage, f.home.path(), "other");
    let answer = f
        .call_as(
            &other,
            "upload.begin",
            f.begin_params(&new_uuid(), "x.png", b"x"),
        )
        .await;
    assert_eq!(result(&answer)["status"], "partial", "{answer}");
}

#[tokio::test]
async fn a_shell_upload_needs_a_live_shell_and_a_cli_that_pastes() {
    let f = Fixture::new();
    f.set("old", "");
    let answer = f
        .call("upload.begin", f.begin_params(&new_uuid(), "a.png", b"a"))
        .await;
    assert_eq!(code(&answer), "cli_error", "{answer}");
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("update RiWork")
    );
    std::fs::remove_file(f.stub.path().join("old")).unwrap();
    f.set("dead", "");
    let answer = f
        .call("upload.begin", f.begin_params(&new_uuid(), "a.png", b"a"))
        .await;
    assert_eq!(code(&answer), "not_found", "{answer}");
    // A chat's inbox needs neither.
    let f = Fixture::without_cli();
    let chat = new_uuid();
    let upload = new_uuid();
    let begun = f
        .call(
            "upload.begin",
            json!({"upload":upload,"chat_id":chat,"name":"IMG_0001","type":"image/jpeg","size":3,"sha256":sha(b"abc")}),
        )
        .await;
    result(&begun);
    result(
        &f.call(
            "upload.chunk",
            json!({"upload":upload,"offset":0,"data":URL_SAFE_NO_PAD.encode(b"abc")}),
        )
        .await,
    );
    let done = f.call("upload.finish", json!({"upload":upload})).await;
    let path = PathBuf::from(result(&done)["path"].as_str().unwrap());
    assert_eq!(
        path.parent().unwrap(),
        f.home.path().join("uploads").join(&chat)
    );
    assert!(path.to_str().unwrap().ends_with(".jpg"));
    // A chat's upload is not a shell's to paste.
    let paste = f
        .call(
            "shell.paste",
            json!({"shell_id":chat,"batch":new_uuid(),"uploads":[upload]}),
        )
        .await;
    assert_eq!(code(&paste), "invalid_request", "{paste}");
}

#[tokio::test]
async fn a_paste_reaches_the_shell_exactly_once_per_batch() {
    let f = Fixture::new();
    let (first, a) = f.send("a.png", b"aaaa").await;
    let (second, b) = f.send("b.txt", b"bbbb").await;
    let batch = new_uuid();
    let paste = json!({"shell_id":f.shell,"batch":batch,"uploads":[first,second]});
    let answer = f.call("shell.paste", paste.clone()).await;
    assert_eq!(result(&answer)["status"], "sent", "{answer}");
    assert_eq!(
        f.paste_calls(),
        vec![vec![
            "shell".to_owned(),
            "paste".to_owned(),
            f.shell.clone(),
            "--".to_owned(),
            a.to_string_lossy().into_owned(),
            b.to_string_lossy().into_owned(),
        ]]
    );
    // A retry with a new request id: nothing is pasted again.
    let again = f.call("shell.paste", paste).await;
    assert_eq!(result(&again)["status"], "duplicate");
    assert_eq!(f.paste_calls().len(), 1);

    // Refused before anything was pasted: the batch is forgotten and may be sent again.
    let batch = new_uuid();
    let paste = json!({"shell_id":f.shell,"batch":batch,"uploads":[first]});
    for (mode, expected) in [
        ("input_unavailable", "input_unavailable"),
        ("not_sent", "cli_error"),
        ("old", "cli_error"),
    ] {
        f.set("mode", mode);
        let answer = f.call("shell.paste", paste.clone()).await;
        assert_eq!(code(&answer), expected, "{mode}: {answer}");
    }
    f.set("mode", "");
    let answer = f.call("shell.paste", paste.clone()).await;
    assert_eq!(result(&answer)["status"], "sent", "{answer}");

    // A failure part way: the batch stays pending and a retry is uncertain, never a second paste.
    let batch = new_uuid();
    let paste = json!({"shell_id":f.shell,"batch":batch,"uploads":[second]});
    f.set("mode", "partial");
    let answer = f.call("shell.paste", paste.clone()).await;
    assert_eq!(code(&answer), "cli_error", "{answer}");
    let calls = f.paste_calls().len();
    f.set("mode", "");
    let retry = f.call("shell.paste", paste).await;
    assert_eq!(result(&retry)["status"], "uncertain");
    assert_eq!(f.paste_calls().len(), calls);

    // Only this device's finished uploads, for this shell.
    let other = pair(&f.rpc.storage, f.home.path(), "other");
    let answer = f
        .call_as(
            &other,
            "shell.paste",
            json!({"shell_id":f.shell,"batch":new_uuid(),"uploads":[first]}),
        )
        .await;
    assert_eq!(code(&answer), "not_found", "{answer}");
    let unfinished = new_uuid();
    result(
        &f.call("upload.begin", f.begin_params(&unfinished, "c.png", b"c"))
            .await,
    );
    let answer = f
        .call(
            "shell.paste",
            json!({"shell_id":f.shell,"batch":new_uuid(),"uploads":[unfinished]}),
        )
        .await;
    assert_eq!(code(&answer), "invalid_request", "{answer}");
}

#[tokio::test]
async fn old_uploads_and_a_revoked_devices_uploads_are_removed() {
    let f = Fixture::new();
    let (_, kept) = f.send("kept.png", b"kept").await;
    let stray = f.inbox().join("stray-00000000.png");
    std::fs::write(&stray, b"stray").unwrap();
    let uploads = Uploads::new(f.rpc.storage.dir.clone());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // Within the day nothing goes.
    uploads.sweep_at(|_| true, now + 60);
    assert!(kept.is_file() && stray.is_file());
    // After it, the upload, the stray file and the empty inbox go.
    uploads.sweep_at(|_| true, now + KEEP_SECONDS + 1);
    assert!(!kept.exists() && !stray.exists());
    assert!(!f.inbox().exists());

    // A partial upload left idle for an hour goes too.
    let idle = new_uuid();
    result(
        &f.call("upload.begin", f.begin_params(&idle, "i.png", b"ii"))
            .await,
    );
    uploads.sweep_at(|_| true, now + upload::IDLE_SECONDS + 1);
    let late = f
        .call(
            "upload.chunk",
            json!({"upload":idle,"offset":0,"data":"aWk"}),
        )
        .await;
    assert_eq!(code(&late), "not_found", "{late}");

    // Revoking the device removes everything it sent.
    let (_, file) = f.send("x.png", b"x").await;
    let partial = new_uuid();
    result(
        &f.call("upload.begin", f.begin_params(&partial, "p.png", b"pp"))
            .await,
    );
    f.rpc.storage.revoke(&f.device).unwrap();
    assert!(!file.exists());
    assert!(
        !f.home
            .path()
            .join("remote/uploads")
            .join(&f.device)
            .exists()
    );
    assert!(
        !f.rpc
            .storage
            .dir
            .join(format!("uploads-{}.json", f.device))
            .exists()
    );
    // And a revoked device may not upload.
    let refused = f
        .rpc
        .handle(
            &f.device,
            req("upload.begin", f.begin_params(&new_uuid(), "y", b"y")),
        )
        .await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_connector_from_before_uploads_is_told_apart() {
    // What an older desktop answers is what the phone checks for: the method is unknown to it.
    // Here the other way round: a method this connector does not know is still refused alike.
    let f = Fixture::without_cli();
    let answer = f.call("upload.begin2", json!({})).await;
    assert_eq!(code(&answer), "invalid_request");
    assert_eq!(answer["error"]["message"], "unsupported RPC method");
    assert_eq!(upload::features()["chunk_bytes"], CHUNK_BYTES);
}
