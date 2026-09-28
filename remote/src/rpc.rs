//! Narrow CLI allowlist plus a durable write-ahead input outcome ledger.
use crate::{
    MAX_PLAINTEXT,
    config::{Storage, private_read, private_write},
    crypto::uuid,
    viewport::Viewport,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex, Weak},
};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    time::{Duration, timeout},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub method: String,
    pub params: Value,
}
#[derive(Debug)]
struct Fault {
    code: &'static str,
    message: String,
}
impl Fault {
    fn new(code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            code,
            message: msg.into(),
        }
    }
}
fn invalid(e: impl std::fmt::Display) -> Fault {
    Fault::new("invalid_request", e.to_string())
}
fn cli_fault(e: impl std::fmt::Display) -> Fault {
    Fault::new("cli_error", e.to_string())
}
/// The CLI wrote more than one encrypted response can carry.
#[derive(Debug)]
struct OutputTooLarge;
impl std::fmt::Display for OutputTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CLI response exceeded limit")
    }
}
impl std::error::Error for OutputTooLarge {}
fn viewport_fault(e: impl std::fmt::Display) -> Fault {
    let message = e.to_string();
    let code = if message.contains("viewport_busy:") {
        "viewport_busy"
    } else if message.contains("viewport_unsupported:") {
        "viewport_unsupported"
    } else {
        "cli_error"
    };
    Fault::new(code, message)
}
pub fn error(id: &str, code: &str, message: impl AsRef<str>) -> Value {
    error_for(json!(id), code, message)
}
// `id` is null only when the request carried no usable ID to correlate with.
fn error_for(id: Value, code: &str, message: impl AsRef<str>) -> Value {
    json!({"v":1,"type":"response","id":id,"ok":false,"error":{"code":code,"message":message.as_ref()}})
}
fn success(id: &str, result: Value) -> Value {
    json!({"v":1,"type":"response","id":id,"ok":true,"result":result})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Project {
    project_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tasks {
    project_id: String,
    worktree_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    shell_id: String,
    lines: Option<u32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    shell_id: String,
    line: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resize {
    shell_id: String,
    columns: u32,
    rows: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Clear {
    shell_id: String,
}
fn params<T: serde::de::DeserializeOwned>(r: &Request) -> std::result::Result<T, Fault> {
    serde_json::from_value(r.params.clone()).map_err(invalid)
}
fn id(s: &str) -> std::result::Result<(), Fault> {
    uuid(s).map(|_| ()).map_err(invalid)
}
fn array(v: Value) -> std::result::Result<Vec<Value>, Fault> {
    v.as_array()
        .cloned()
        .ok_or_else(|| cli_fault("CLI did not return an array"))
}
fn project(v: Value, fields: &[&str]) -> Value {
    let mut m = serde_json::Map::new();
    for f in fields {
        if let Some(x) = v.get(*f) {
            m.insert((*f).into(), x.clone());
        }
    }
    Value::Object(m)
}
const SESSION_FIELDS: &[&str] = &[
    "id",
    "project_id",
    "worktree_id",
    "kind",
    "cwd",
    "harness",
    "alive",
    "created_at_unix",
];
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    digest: String,
    response: Option<Value>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    entries: BTreeMap<String, Record>,
}

pub struct Rpc {
    pub cli: PathBuf,
    pub storage: Storage,
    input_locks: Mutex<BTreeMap<String, Weak<tokio::sync::Mutex<()>>>>,
}
impl Rpc {
    pub fn new(cli: PathBuf, storage: Storage) -> Self {
        Self {
            cli,
            storage,
            input_locks: Mutex::new(BTreeMap::new()),
        }
    }
    fn input_lock(&self, shell: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.input_locks.lock().expect("input lock registry");
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(shell).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(shell.into(), Arc::downgrade(&lock));
        lock
    }
    async fn raw(&self, args: Vec<String>) -> Result<Vec<u8>> {
        let mut child = Command::new(&self.cli)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("start configured RiWork CLI {}", self.cli.display()))?;
        let stdout = child.stdout.take().context("stdout")?;
        let stderr = child.stderr.take().context("stderr")?;
        let run = async {
            let out = async {
                let mut b = vec![];
                stdout
                    .take((MAX_PLAINTEXT + 1) as u64)
                    .read_to_end(&mut b)
                    .await?;
                ensure!(b.len() <= MAX_PLAINTEXT, OutputTooLarge);
                Ok::<_, anyhow::Error>(b)
            };
            let err = async {
                let mut b = vec![];
                stderr.take(4097).read_to_end(&mut b).await?;
                ensure!(b.len() <= 4096, "CLI diagnostics exceeded limit");
                Ok::<_, anyhow::Error>(b)
            };
            let (out, err, status) = tokio::try_join!(out, err, async {
                Ok::<_, anyhow::Error>(child.wait().await?)
            })?;
            ensure!(
                status.success(),
                "RiWork CLI failed: {}",
                String::from_utf8_lossy(&err).trim()
            );
            Ok(out)
        };
        timeout(Duration::from_secs(15), run)
            .await
            .context("RiWork CLI timeout")?
    }
    async fn read(&self, args: &[&str]) -> std::result::Result<Value, Fault> {
        let mut a: Vec<String> = args.iter().map(|x| (*x).to_owned()).collect();
        a.push("--json".into());
        let data = self.raw(a).await.map_err(|e| {
            if e.is::<OutputTooLarge>() {
                Fault::new(
                    "response_too_large",
                    "CLI output exceeds the encrypted response limit; reduce output lines",
                )
            } else {
                cli_fault(e)
            }
        })?;
        serde_json::from_slice(&data).map_err(cli_fault)
    }
    async fn sessions(&self) -> std::result::Result<Vec<Value>, Fault> {
        let mut v = array(self.read(&["shell", "list", "--all"]).await?)?;
        v.extend(array(self.read(&["orchestrator", "list"]).await?)?);
        Ok(v)
    }
    async fn selected(&self, shell: &str) -> std::result::Result<(), Fault> {
        id(shell)?;
        let list = self.sessions().await?;
        let Some(s) = list
            .iter()
            .find(|s| s.get("id").and_then(Value::as_str) == Some(shell))
        else {
            return Err(Fault::new("not_found", "existing shell ID not found"));
        };
        if s.get("alive").and_then(Value::as_bool) != Some(true) {
            return Err(Fault::new("not_found", "selected shell is not alive"));
        }
        Ok(())
    }
    pub async fn handle(&self, device: &str, value: Value) -> Result<Value> {
        self.handle_in(device, value, None).await
    }
    pub async fn handle_in(
        &self,
        device: &str,
        value: Value,
        viewport: Option<&mut Viewport>,
    ) -> Result<Value> {
        // Malformed requests get an error response, not a dropped session. Only
        // an ID that is a bounded string can be echoed for correlation.
        let request_id = match value.get("id") {
            Some(Value::String(s)) if s.len() <= 64 => s.clone(),
            _ => {
                return Ok(error_for(
                    Value::Null,
                    "invalid_request",
                    "request must be a JSON object with a string id of at most 64 bytes",
                ));
            }
        };
        let request: Request = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(e) => return Ok(error(&request_id, "invalid_request", e.to_string())),
        };
        let result = if request.v != 1 || request.kind != "request" {
            Err(invalid("unsupported request version/type"))
        } else if let Err(e) = id(&request.id) {
            Err(e)
        } else if !self.storage.authorized(device)? {
            return Err(anyhow::anyhow!("device revoked"));
        } else {
            let ledger_path = self.storage.dir.join(format!("outcomes-{device}.json"));
            if request.method != "shell.input" && ledger_path.exists() {
                let ledger: Ledger = private_read(&ledger_path, 64 * 1024 * 1024)?;
                if ledger.entries.contains_key(&request.id) {
                    return Ok(error(
                        &request.id,
                        "request_conflict",
                        "request UUID already used for input",
                    ));
                }
            }
            self.dispatch(device, &request, viewport).await
        };
        let response = match result {
            Ok(v) => success(&request_id, v),
            Err(f) => error(&request_id, f.code, f.message),
        };
        if serde_json::to_vec(&response)?.len() > MAX_PLAINTEXT {
            return Ok(error(
                &request_id,
                "response_too_large",
                "result exceeds encrypted response limit; reduce output lines",
            ));
        }
        Ok(response)
    }
    async fn dispatch(
        &self,
        device: &str,
        r: &Request,
        viewport: Option<&mut Viewport>,
    ) -> std::result::Result<Value, Fault> {
        match r.method.as_str() {
            "projects.list" => {
                let _: Empty = params(r)?;
                let v = array(self.read(&["project", "list"]).await?)?
                    .into_iter()
                    .map(|v| project(v, &["id", "name", "root", "created_at"]))
                    .collect::<Vec<_>>();
                Ok(json!({"projects":v}))
            }
            "worktrees.list" => {
                let p: Project = params(r)?;
                id(&p.project_id)?;
                let v = array(
                    self.read(&["worktree", "list", "--project", &p.project_id])
                        .await?,
                )?
                .into_iter()
                .map(|v| {
                    project(
                        v,
                        &[
                            "id",
                            "project_id",
                            "branch",
                            "path",
                            "is_primary",
                            "created_at",
                        ],
                    )
                })
                .collect::<Vec<_>>();
                Ok(json!({"worktrees":v}))
            }
            "tasks.list" => {
                let p: Tasks = params(r)?;
                id(&p.project_id)?;
                let tasks = if let Some(w) = p.worktree_id {
                    id(&w)?;
                    let trees = array(
                        self.read(&["worktree", "list", "--project", &p.project_id])
                            .await?,
                    )?;
                    if !trees
                        .iter()
                        .any(|t| t.get("id").and_then(Value::as_str) == Some(&w))
                    {
                        return Err(Fault::new(
                            "not_found",
                            "worktree does not belong to selected project",
                        ));
                    }
                    self.read(&["task", "list", "--worktree", &w]).await?
                } else {
                    self.read(&["task", "list", "--project", &p.project_id])
                        .await?
                };
                Ok(
                    json!({"tasks":array(tasks)?.into_iter().map(|v|project(v,&["id","project_id","title","details","status","worktree_id","created_at","updated_at"])).collect::<Vec<_>>()}),
                )
            }
            "shells.list" => {
                let p: Project = params(r)?;
                id(&p.project_id)?;
                Ok(
                    json!({"shells":array(self.read(&["shell","list","--project",&p.project_id]).await?)?.into_iter().map(|v|project(v,SESSION_FIELDS)).collect::<Vec<_>>()}),
                )
            }
            "orchestrators.list" => {
                let _: Empty = params(r)?;
                Ok(
                    json!({"orchestrators":array(self.read(&["orchestrator","list"]).await?)?.into_iter().map(|v|project(v,SESSION_FIELDS)).collect::<Vec<_>>()}),
                )
            }
            "shell.output" => {
                let p: Output = params(r)?;
                let lines = p.lines.unwrap_or(200);
                if !(1..=2000).contains(&lines) {
                    return Err(invalid("lines must be 1..2000"));
                }
                self.selected(&p.shell_id).await?;
                let v = self
                    .read(&[
                        "shell",
                        "output",
                        &p.shell_id,
                        "--lines",
                        &lines.to_string(),
                    ])
                    .await?;
                let text = v
                    .get("output")
                    .and_then(Value::as_str)
                    .ok_or_else(|| cli_fault("missing CLI output"))?;
                Ok(json!({"shell_id":p.shell_id,"output":text}))
            }
            "shell.resize" => {
                let p: Resize = params(r)?;
                id(&p.shell_id)?;
                if !(20..=300).contains(&p.columns) || !(8..=160).contains(&p.rows) {
                    return Err(invalid("columns must be 20..300 and rows 8..160"));
                }
                let v = viewport.ok_or_else(|| invalid("authenticated connection required"))?;
                self.selected(&p.shell_id).await?;
                if v.selected
                    .as_ref()
                    .is_some_and(|(s, _, _)| *s != p.shell_id)
                {
                    self.clear_viewport(v).await.map_err(viewport_fault)?;
                }
                // Track before invoking CLI, so cancellation during the command
                // still clears any geometry it may have applied.
                v.selected = Some((p.shell_id.clone(), p.columns, p.rows));
                if let Err(e) = self.renew_viewport(v).await {
                    let fault = viewport_fault(e);
                    if fault.code == "viewport_busy" {
                        v.selected = None; // denied ownership must not be renewed
                    } else {
                        let _ = self.clear_viewport(v).await;
                    }
                    return Err(fault);
                }
                Ok(json!({"shell_id":p.shell_id,"columns":p.columns,"rows":p.rows}))
            }
            "shell.resize.clear" => {
                let p: Clear = params(r)?;
                id(&p.shell_id)?;
                let v = viewport.ok_or_else(|| invalid("authenticated connection required"))?;
                // The CLI creates a lock file per shell ID it is asked about, so an
                // arbitrary UUID must not reach it. A shell this connection pinned
                // may have died since; clearing it must still work.
                if v.selected.as_ref().is_none_or(|(s, _, _)| *s != p.shell_id) {
                    self.selected(&p.shell_id).await?;
                }
                self.raw(v.args("resize-clear", &p.shell_id))
                    .await
                    .map_err(viewport_fault)?;
                if v.selected
                    .as_ref()
                    .is_some_and(|(s, _, _)| *s == p.shell_id)
                {
                    v.selected = None;
                }
                Ok(json!({"shell_id":p.shell_id,"status":"cleared"}))
            }
            "shell.input" => {
                let p: Input = params(r)?;
                id(&p.shell_id)?;
                if p.line.len() > 8192
                    || p.line
                        .chars()
                        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
                {
                    return Err(invalid(
                        "input must be one physical line <=8192 bytes without controls",
                    ));
                }
                self.input(device, r, p).await
            }
            _ => Err(invalid("unsupported RPC method")),
        }
    }
    pub async fn renew_viewport(&self, v: &Viewport) -> Result<()> {
        if let Some((shell, columns, rows)) = &v.selected {
            let mut args = v.args("resize", shell);
            args.extend([
                "--columns".into(),
                columns.to_string(),
                "--rows".into(),
                rows.to_string(),
            ]);
            self.raw(args).await?;
        }
        Ok(())
    }
    pub async fn clear_viewport(&self, v: &mut Viewport) -> Result<()> {
        if let Some((shell, _, _)) = &v.selected {
            self.raw(v.args("resize-clear", shell)).await?;
        }
        v.selected = None;
        Ok(())
    }
    async fn input(
        &self,
        device: &str,
        r: &Request,
        p: Input,
    ) -> std::result::Result<Value, Fault> {
        id(device)?;
        // Only one connector per home and one RPC loop per device; this lock also
        // protects external recovery tooling and tests from racing input writes.
        let _lock = self
            .storage
            .lock(&format!("outcomes-{device}.lock"))
            .map_err(cli_fault)?;
        let path = self.storage.dir.join(format!("outcomes-{device}.json"));
        let mut ledger: Ledger = if path.exists() {
            private_read(&path, 64 * 1024 * 1024).map_err(cli_fault)?
        } else {
            Ledger::default()
        };
        let digest = hex::encode(Sha256::digest(serde_json::to_vec(r).map_err(cli_fault)?));
        if let Some(record) = ledger.entries.get(&r.id) {
            if record.digest != digest {
                return Err(Fault::new(
                    "request_conflict",
                    "input request UUID already used with different contents",
                ));
            }
            if let Some(response) = &record.response {
                if response.get("ok") == Some(&Value::Bool(true)) {
                    return Ok(response["result"].clone());
                }
                return Err(Fault::new(
                    "outcome_unknown",
                    response.get("error").and_then(|e| e.get("message")).and_then(Value::as_str)
                        .unwrap_or("previous send outcome is unknown; inspect the selected shell before any manual retry"),
                ));
            }
            return Err(Fault::new(
                "outcome_unknown",
                "pending send outcome is unknown; never automatically resend with a new UUID",
            ));
        }
        if ledger.entries.len() >= 4096 {
            return Err(Fault::new(
                "cache_full",
                "input outcome cache full; review outcomes then pair a new device",
            ));
        }
        // Await per-shell serialization across every paired device. The root
        // CLI also locks the complete paste/Return transaction across processes.
        let shell_lock = self.input_lock(&p.shell_id);
        let _shell_guard = shell_lock.lock().await;
        self.selected(&p.shell_id).await?;
        if !self.storage.authorized(device).map_err(cli_fault)? {
            return Err(Fault::new("not_found", "device revoked"));
        }
        ledger.entries.insert(
            r.id.clone(),
            Record {
                digest,
                response: None,
            },
        );
        private_write(&path, &ledger).map_err(cli_fault)?; // durable before irreversible submission
        let sent = self
            .raw(vec![
                "shell".into(),
                "send".into(),
                p.shell_id.clone(),
                p.line,
            ])
            .await;
        let result = match sent {
            Ok(_) => Ok(json!({"shell_id":p.shell_id,"status":"sent"})),
            Err(_) => Err(Fault::new(
                "outcome_unknown",
                "CLI submission outcome unknown; inspect selected shell; do not automatically resend",
            )),
        };
        let response = match &result {
            Ok(v) => success(&r.id, v.clone()),
            Err(f) => error(&r.id, f.code, &f.message),
        };
        ledger
            .entries
            .get_mut(&r.id)
            .expect("inserted record")
            .response = Some(response);
        if private_write(&path, &ledger).is_err() {
            return Err(Fault::new(
                "outcome_unknown",
                "submission attempted but outcome persistence failed; do not resend",
            ));
        }
        result
    }
}
