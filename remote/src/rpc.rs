//! Narrow CLI allowlist plus a durable write-ahead input outcome ledger.
use crate::{
    MAX_PLAINTEXT, appearance,
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
    styled: Option<bool>,
    if_changed: Option<String>,
    wait_ms: Option<i64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    shell_id: String,
    line: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Keys {
    shell_id: String,
    batch: String,
    items: Vec<Value>,
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
/// The longest a `shell.output` may wait for a change.
pub const MAX_WAIT_MS: i64 = 10_000;
/// A CLI call that is not a wait. Above the tmux timeout of a single capture.
const CLI_TIMEOUT: Duration = Duration::from_secs(15);
/// What a waiting CLI call may take beyond its wait: the captures around it.
const CLI_WAIT_MARGIN: Duration = Duration::from_secs(8);

/// How long the CLI may take for a `shell.output` that waits up to `wait_ms`:
/// the wait plus the captures around it, and never less than any other call.
/// The CLI itself ends within about `wait_ms`, so this only trips when it hangs.
fn cli_limit(wait_ms: i64) -> Duration {
    CLI_TIMEOUT.max(Duration::from_millis(wait_ms.clamp(0, MAX_WAIT_MS) as u64) + CLI_WAIT_MARGIN)
}

/// A hash as `shell.output` hands them out: short, printable, no spaces.
fn hash_shaped(hash: &str) -> bool {
    (1..=64).contains(&hash.len()) && hash.bytes().all(|b| b.is_ascii_graphic())
}
/// Whether `text` holds no escape but well-formed SGR sequences
/// (`ESC [ digits ; : m`) and no control character but the newline and tab.
/// The CLI filters its styled output the same way; this refuses to pass on
/// what a CLI that did not.
fn sgr_only(text: &str) -> bool {
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                if chars.next() != Some('[') {
                    return false;
                }
                let mut length = 0;
                loop {
                    match chars.next() {
                        Some('m') => break,
                        Some(c) if c.is_ascii_digit() || c == ';' || c == ':' => length += 1,
                        _ => return false,
                    }
                    if length > 64 {
                        return false;
                    }
                }
            }
            '\n' | '\t' => {}
            c if c.is_control() => return false,
            _ => {}
        }
    }
    true
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

/// Direct-typing limits. `src/session_keys.rs` enforces the same ones in the
/// CLI; the two test matrices must stay identical.
const KEYS_MAX_ITEMS: usize = 64;
const KEYS_MAX_TEXT_BYTES: usize = 4096;
const KEY_NAMES: [&str; 14] = [
    "Enter",
    "Tab",
    "BTab",
    "Escape",
    "Backspace",
    "Delete",
    "Up",
    "Down",
    "Left",
    "Right",
    "Home",
    "End",
    "PageUp",
    "PageDown",
];
/// Batches remembered per device, oldest pruned first.
pub const KEYS_LEDGER_MAX: usize = 4096;

fn valid_key(name: &str) -> bool {
    KEY_NAMES.contains(&name) || matches!(name.as_bytes(), [b'C', b'-', b'a'..=b'z'])
}
/// One `shell.keys` item as the CLI's argv encoding: `t:TEXT` or `k:KEY`.
fn key_item(index: usize, item: &Value) -> std::result::Result<(String, usize), Fault> {
    let n = index + 1;
    let object = item
        .as_object()
        .ok_or_else(|| invalid(format!("item {n} must be an object")))?;
    if object.keys().any(|k| k != "text" && k != "key") {
        return Err(invalid(format!("item {n} takes only \"text\" or \"key\"")));
    }
    match (object.get("text"), object.get("key")) {
        (Some(Value::String(text)), None) => {
            if text.is_empty() || text.len() > KEYS_MAX_TEXT_BYTES {
                return Err(invalid(format!(
                    "item {n}: text must be 1..={KEYS_MAX_TEXT_BYTES} bytes"
                )));
            }
            if text
                .chars()
                .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
            {
                return Err(invalid(format!(
                    "item {n}: text must not contain control characters; send a key for Enter or Tab"
                )));
            }
            Ok((format!("t:{text}"), text.len()))
        }
        (None, Some(Value::String(key))) => {
            if !valid_key(key) {
                return Err(invalid(format!("item {n}: unknown key")));
            }
            Ok((format!("k:{key}"), 0))
        }
        (Some(_), Some(_)) => Err(invalid(format!(
            "item {n} takes either \"text\" or \"key\", not both"
        ))),
        (None, None) => Err(invalid(format!("item {n} needs a \"text\" or a \"key\""))),
        _ => Err(invalid(format!("item {n}: text and key must be strings"))),
    }
}
/// The whole batch as CLI items: 1..=64 items, at most 4096 text bytes.
fn key_items(items: &[Value]) -> std::result::Result<Vec<String>, Fault> {
    if items.is_empty() || items.len() > KEYS_MAX_ITEMS {
        return Err(invalid(format!(
            "items must hold 1..={KEYS_MAX_ITEMS} entries"
        )));
    }
    let mut total = 0usize;
    let mut argv = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let (encoded, text_bytes) = key_item(index, item)?;
        total += text_bytes;
        argv.push(encoded);
    }
    if total > KEYS_MAX_TEXT_BYTES {
        return Err(invalid(format!(
            "items may hold at most {KEYS_MAX_TEXT_BYTES} text bytes in all"
        )));
    }
    Ok(argv)
}
/// What a failed `riwork shell keys` says about the batch. A CLI error that
/// begins with one of its tokens happened before any key was typed, so the
/// batch can be forgotten and retried; anything else may have typed part of it.
/// Only the first line of stderr counts: text a shell echoes cannot forge it.
fn keys_fault(error: &anyhow::Error) -> (Fault, bool) {
    let message = error.to_string();
    let detail = message.strip_prefix("RiWork CLI failed: riwork: ");
    let tokens: [(&str, &'static str); 4] = [
        ("input_unavailable: ", "input_unavailable"),
        ("not_found: ", "not_found"),
        ("invalid_request: ", "invalid_request"),
        ("not_sent: ", "cli_error"),
    ];
    if let Some(detail) = detail {
        // A CLI from before direct typing has no `shell keys`: nothing was typed.
        if detail.starts_with("Unknown shell command 'keys'") {
            return (
                Fault::new(
                    "cli_error",
                    "the installed riwork CLI does not support shell keys; update RiWork",
                ),
                true,
            );
        }
        for (token, code) in tokens {
            if let Some(rest) = detail.strip_prefix(token) {
                return (
                    Fault::new(code, rest.lines().next().unwrap_or_default()),
                    true,
                );
            }
        }
    }
    (
        Fault::new(
            "cli_error",
            format!(
                "{message}; the batch may be partly typed, so repeating its batch UUID reports uncertain"
            ),
        ),
        false,
    )
}
fn appearance_not_published() -> Fault {
    Fault::new("not_found", "appearance not published")
}
/// What a failed `riwork appearance --json` says. The CLI reports a missing or
/// invalid file with one message; output past the document limit is invalid too.
fn appearance_fault(error: &anyhow::Error) -> Fault {
    if error.is::<OutputTooLarge>() {
        return appearance_not_published();
    }
    let message = error.to_string();
    if let Some(detail) = message.strip_prefix("RiWork CLI failed: riwork: ") {
        if detail.starts_with("RiWork has not published its appearance yet") {
            return appearance_not_published();
        }
        // A CLI from before theme sync has no `appearance` command.
        if detail.starts_with("'appearance' is not a riwork command") {
            return Fault::new(
                "cli_error",
                "the installed riwork CLI does not support appearance; update RiWork",
            );
        }
    }
    cli_fault(error)
}
/// The screen fields of the CLI's `shell output`, all or none: a cursor that
/// does not land on the last `rows` lines of the text is not passed on.
fn screen_fields(cli: &Value, output: &str) -> Option<Value> {
    let number = |v: &Value| v.as_u64().and_then(|n| u32::try_from(n).ok());
    let cursor = cli.get("cursor")?;
    let (x, y) = (number(cursor.get("x")?)?, number(cursor.get("y")?)?);
    let (rows, cols) = (number(cli.get("rows")?)?, number(cli.get("cols")?)?);
    let in_mode = cli.get("in_mode")?.as_bool()?;
    if rows == 0 || cols == 0 || y >= rows || output.lines().count() < rows as usize {
        return None;
    }
    Some(json!({"cursor":{"x":x,"y":y},"rows":rows,"cols":cols,"in_mode":in_mode}))
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BatchState {
    /// Recorded before typing. If it is still pending, the outcome is unknown.
    Pending,
    Sent,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchRecord {
    batch: String,
    state: BatchState,
}
/// Direct-typing batches of one device, oldest first, in `keys-DEVICE.json`.
/// Deliberately not the input outcome ledger: batches are keyed by their own
/// UUID, carry no request digest, and are pruned instead of filling up.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchLedger {
    batches: Vec<BatchRecord>,
}

/// How a request reaches the viewport of its connection.
enum ViewportAccess<'a> {
    /// No connection behind the request: resizing is refused.
    Absent,
    /// The caller has the viewport to itself for this one call.
    Owned(&'a mut Viewport),
    /// Concurrent requests share it. `None` once the connection is over.
    Shared(&'a tokio::sync::Mutex<Option<Viewport>>),
}
impl<'a> ViewportAccess<'a> {
    async fn hold(self) -> HeldViewport<'a> {
        match self {
            Self::Absent => HeldViewport::Absent,
            Self::Owned(viewport) => HeldViewport::Owned(viewport),
            Self::Shared(lock) => HeldViewport::Shared(lock.lock().await),
        }
    }
}
enum HeldViewport<'a> {
    Absent,
    Owned(&'a mut Viewport),
    Shared(tokio::sync::MutexGuard<'a, Option<Viewport>>),
}
impl HeldViewport<'_> {
    fn get(&mut self) -> Option<&mut Viewport> {
        match self {
            Self::Absent => None,
            Self::Owned(viewport) => Some(viewport),
            Self::Shared(guard) => guard.as_mut(),
        }
    }
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
        self.raw_within(args, CLI_TIMEOUT).await
    }
    async fn raw_within(&self, args: Vec<String>, limit: Duration) -> Result<Vec<u8>> {
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
        timeout(limit, run).await.context("RiWork CLI timeout")?
    }
    async fn read(&self, args: &[&str]) -> std::result::Result<Value, Fault> {
        self.read_within(args.iter().map(|x| (*x).to_owned()).collect(), CLI_TIMEOUT)
            .await
    }
    async fn read_within(
        &self,
        mut a: Vec<String>,
        limit: Duration,
    ) -> std::result::Result<Value, Fault> {
        a.push("--json".into());
        let data = self.raw_within(a, limit).await.map_err(|e| {
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
        self.handle_with(device, value, ViewportAccess::Absent)
            .await
    }
    /// One request on a connection that owns its viewport for the call.
    pub async fn handle_in(
        &self,
        device: &str,
        value: Value,
        viewport: Option<&mut Viewport>,
    ) -> Result<Value> {
        let viewport = viewport.map_or(ViewportAccess::Absent, ViewportAccess::Owned);
        self.handle_with(device, value, viewport).await
    }
    /// One request among concurrent ones on a connection: only the methods
    /// that resize take the viewport lock, and hold it while they run.
    pub async fn handle_shared(
        &self,
        device: &str,
        value: Value,
        viewport: &tokio::sync::Mutex<Option<Viewport>>,
    ) -> Result<Value> {
        self.handle_with(device, value, ViewportAccess::Shared(viewport))
            .await
    }
    async fn handle_with(
        &self,
        device: &str,
        value: Value,
        viewport: ViewportAccess<'_>,
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
        viewport: ViewportAccess<'_>,
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
                let wait_ms = p.wait_ms.unwrap_or(0);
                if !(0..=MAX_WAIT_MS).contains(&wait_ms) {
                    return Err(invalid(format!("wait_ms must be 0..{MAX_WAIT_MS}")));
                }
                if p.if_changed.as_deref().is_some_and(|h| !hash_shaped(h)) {
                    return Err(invalid(
                        "if_changed must be the hash of an earlier result: 1..64 printable characters",
                    ));
                }
                let styled = p.styled.unwrap_or(false);
                self.selected(&p.shell_id).await?;
                let mut args: Vec<String> = [
                    "shell",
                    "output",
                    p.shell_id.as_str(),
                    "--lines",
                    &lines.to_string(),
                ]
                .into_iter()
                .map(String::from)
                .collect();
                if styled {
                    args.push("--styled".into());
                }
                let mut limit = CLI_TIMEOUT;
                if let Some(hash) = &p.if_changed {
                    // `=`: whatever the hash looks like, it stays one value.
                    args.push(format!("--if-changed={hash}"));
                    // A wait without a hash to compare has nothing to wait for.
                    args.push("--wait-ms".into());
                    args.push(wait_ms.to_string());
                    limit = cli_limit(wait_ms);
                }
                let v = self.read_within(args, limit).await.map_err(|fault| {
                    // A CLI from before styled output and waiting refuses the flags.
                    if fault.code == "cli_error"
                        && fault.message.contains("Usage: riwork shell output")
                        && (styled || p.if_changed.is_some())
                    {
                        Fault::new(
                            "cli_error",
                            "the installed riwork CLI does not support styled output or waiting for changes; update RiWork",
                        )
                    } else {
                        fault
                    }
                })?;
                let hash = v
                    .get("hash")
                    .and_then(Value::as_str)
                    .filter(|h| hash_shaped(h));
                if v.get("unchanged") == Some(&Value::Bool(true)) {
                    // Only ever the answer to a hash this request sent.
                    return match (&p.if_changed, hash) {
                        (Some(asked), Some(hash)) if asked == hash => {
                            Ok(json!({"shell_id":p.shell_id,"unchanged":true,"hash":hash}))
                        }
                        _ => Err(cli_fault(
                            "CLI reported unchanged output that was not asked",
                        )),
                    };
                }
                let text = v
                    .get("output")
                    .and_then(Value::as_str)
                    .ok_or_else(|| cli_fault("missing CLI output"))?;
                if styled && !sgr_only(text) {
                    return Err(cli_fault(
                        "CLI returned escape sequences other than SGR for styled output",
                    ));
                }
                let mut result = json!({"shell_id":p.shell_id,"output":text});
                // Additive: the last `rows` lines of `output` are the visible
                // screen. Absent when the desktop could not read the pane.
                if let (Some(screen), Some(map)) = (screen_fields(&v, text), result.as_object_mut())
                    && let Some(fields) = screen.as_object()
                {
                    map.extend(fields.clone());
                }
                // Additive: names this exact answer, for `if_changed`.
                if let Some(hash) = hash {
                    result["hash"] = json!(hash);
                }
                Ok(result)
            }
            "shell.resize" => {
                let p: Resize = params(r)?;
                id(&p.shell_id)?;
                if !(20..=300).contains(&p.columns) || !(8..=160).contains(&p.rows) {
                    return Err(invalid("columns must be 20..300 and rows 8..160"));
                }
                let mut held = viewport.hold().await;
                let v = held
                    .get()
                    .ok_or_else(|| invalid("authenticated connection required"))?;
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
                let mut held = viewport.hold().await;
                let v = held
                    .get()
                    .ok_or_else(|| invalid("authenticated connection required"))?;
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
            "appearance.get" => {
                let _: Empty = params(r)?;
                self.appearance().await
            }
            "shell.keys" => {
                let p: Keys = params(r)?;
                id(&p.shell_id)?;
                id(&p.batch)?;
                let items = key_items(&p.items)?;
                self.keys(device, p, items).await
            }
            _ => Err(invalid("unsupported RPC method")),
        }
    }
    /// The colors the desktop published for the phone: read-only, no shell.
    async fn appearance(&self) -> std::result::Result<Value, Fault> {
        let data = self
            .raw(vec!["appearance".into(), "--json".into()])
            .await
            .map_err(|e| appearance_fault(&e))?;
        appearance::validate(&data).ok_or_else(appearance_not_published)
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
        // Requests of one device run concurrently, but the file lock below is
        // a try-lock: without this, a second input would fail instead of wait.
        let device_lock = self.input_lock(&format!("input-{device}"));
        let _device_guard = device_lock.lock().await;
        // Only one connector per home; this lock also
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
    /// Type one batch into a shell, exactly once per (device, batch UUID).
    async fn keys(
        &self,
        device: &str,
        p: Keys,
        items: Vec<String>,
    ) -> std::result::Result<Value, Fault> {
        id(device)?;
        // One batch at a time per device, so a retry that overlaps its own
        // first attempt waits for it and then reports `duplicate`. The file
        // lock keeps external recovery tooling out of the ledger meanwhile.
        let device_lock = self.input_lock(&format!("keys-{device}"));
        let _device_guard = device_lock.lock().await;
        let _lock = self
            .storage
            .lock(&format!("keys-{device}.lock"))
            .map_err(cli_fault)?;
        let path = self.storage.dir.join(format!("keys-{device}.json"));
        let mut ledger: BatchLedger = if path.exists() {
            private_read(&path, 8 * 1024 * 1024).map_err(cli_fault)?
        } else {
            BatchLedger::default()
        };
        let reply =
            |status: &str| Ok(json!({"shell_id":p.shell_id,"batch":p.batch,"status":status}));
        // A repeat is answered from the ledger alone, before any CLI runs.
        if let Some(record) = ledger.batches.iter().find(|r| r.batch == p.batch) {
            return reply(match record.state {
                BatchState::Sent => "duplicate",
                BatchState::Pending => "uncertain",
            });
        }
        let shell_lock = self.input_lock(&p.shell_id);
        let _shell_guard = shell_lock.lock().await;
        self.selected(&p.shell_id).await?;
        if !self.storage.authorized(device).map_err(cli_fault)? {
            return Err(Fault::new("not_found", "device revoked"));
        }
        let excess = (ledger.batches.len() + 1).saturating_sub(KEYS_LEDGER_MAX);
        ledger.batches.drain(..excess);
        ledger.batches.push(BatchRecord {
            batch: p.batch.clone(),
            state: BatchState::Pending,
        });
        private_write(&path, &ledger).map_err(cli_fault)?; // durable before typing
        let mut args: Vec<String> = ["shell", "keys", p.shell_id.as_str(), "--"]
            .into_iter()
            .map(String::from)
            .collect();
        args.extend(items);
        match self.raw(args).await {
            Ok(_) => {
                if let Some(record) = ledger.batches.last_mut() {
                    record.state = BatchState::Sent;
                }
                // The keys are typed either way; if this write fails the batch
                // stays pending and a repeat reports uncertain, never a resend.
                let _ = private_write(&path, &ledger);
                reply("sent")
            }
            Err(error) => {
                let (fault, not_sent) = keys_fault(&error);
                if not_sent {
                    // Nothing was typed: forget the batch so a retry can send it.
                    ledger.batches.pop();
                    let _ = private_write(&path, &ledger);
                }
                Err(fault)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_waiting_cli_gets_the_wait_plus_a_margin_and_never_less_than_other_calls() {
        assert_eq!(cli_limit(0), Duration::from_secs(15));
        assert_eq!(cli_limit(5_000), Duration::from_secs(15));
        assert_eq!(cli_limit(7_000), Duration::from_secs(15));
        assert_eq!(cli_limit(8_000), Duration::from_secs(16));
        assert_eq!(cli_limit(MAX_WAIT_MS), Duration::from_secs(18));
        // The longest allowed wait leaves at least the margin (two tmux
        // calls' worth of slack) before the connector gives up on the CLI.
        assert!(
            cli_limit(MAX_WAIT_MS)
                >= Duration::from_millis(MAX_WAIT_MS as u64) + Duration::from_secs(5)
        );
        // Out of range never widens the limit.
        assert_eq!(cli_limit(i64::MAX), cli_limit(MAX_WAIT_MS));
        assert_eq!(cli_limit(-1), Duration::from_secs(15));
    }

    #[test]
    fn hashes_are_short_and_printable() {
        for good in ["0123456789abcdef", "a", &"x".repeat(64), "A-b_c.d~"] {
            assert!(hash_shaped(good), "{good}");
        }
        for bad in [
            "",
            &"x".repeat(65),
            "a b",
            "a\tb",
            "a\nb",
            "\u{e9}",
            "a\u{7f}",
            "a\u{1b}",
        ] {
            assert!(!hash_shaped(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_well_formed_sgr_passes_the_styled_check() {
        for good in [
            "",
            "plain\n\ttext \u{e9}",
            "\u{1b}[m",
            "\u{1b}[0m",
            "\u{1b}[1;31mred\u{1b}[0m\nnext",
            "\u{1b}[38;5;200m\u{1b}[38;2;1;2;3m\u{1b}[38:2::1:2:3m",
            &format!("\u{1b}[{}m", "1".repeat(64)),
        ] {
            assert!(sgr_only(good), "{good:?}");
        }
        for bad in [
            "\u{1b}",
            "\u{1b}[",
            "\u{1b}[31",
            "\u{1b}[2J",
            "\u{1b}[?25l",
            "\u{1b}[>4;2m",
            "\u{1b}[1 m",
            "\u{1b}]0;title\u{7}",
            "\u{1b}]8;;u\u{1b}\\",
            "\u{1b}(0",
            "\u{1b}7",
            "\u{1b}[31\u{1b}[0m",
            &format!("\u{1b}[{}m", "1".repeat(65)),
            "a\rb",
            "a\u{7}b",
            "a\u{e}b",
            "a\u{0}b",
            "a\u{7f}b",
            "a\u{9b}31mb",
            "a\u{85}b",
        ] {
            assert!(!sgr_only(bad), "{bad:?}");
        }
    }
}
