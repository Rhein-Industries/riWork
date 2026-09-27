//! Narrow CLI allowlist plus a durable write-ahead input outcome ledger.
use crate::{
    MAX_PLAINTEXT,
    config::{Storage, private_read, private_write},
    crypto::uuid,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf, process::Stdio};
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
pub fn error(id: &str, code: &str, message: impl AsRef<str>) -> Value {
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
}
impl Rpc {
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
                ensure!(b.len() <= MAX_PLAINTEXT, "CLI response exceeded limit");
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
        let data = self.raw(a).await.map_err(cli_fault)?;
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
        let request_id = value
            .get("id")
            .and_then(Value::as_str)
            .context("request ID missing")?
            .to_owned();
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
            self.dispatch(device, &request).await
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
    async fn dispatch(&self, device: &str, r: &Request) -> std::result::Result<Value, Fault> {
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
