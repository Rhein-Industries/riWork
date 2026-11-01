//! `native.invoke` and `state.watch` ("Native invoke extension" and "State watch extension" in
//! `docs/remote-protocol.md`): the desktop's own `riwork/native` methods and a wait for their
//! data to change, so a paired device's screens get what the Mac's screens get.
//!
//! `native.invoke {module, method, args}` is `riwork native invoke MODULE.METHOD --json` with
//! `args` on the CLI's standard input, answered `{value}` (and `page` for a directory listing).
//! Only the methods of [`METHODS`] exist here, and of those only the ones the installed CLI
//! names in `riwork capabilities --json` (`native.methods`); `ready.features.native` lists what
//! is left. Writes run in the ordered lane, reads in a shared one ([`is_write`]), and a write's
//! CLI outlives the connection as a creation's does (see `Rpc::native_invoke`). The CLI holds
//! the typed arguments and the argument policy (`crates/riwork-nsh-native/src/invoke.rs`);
//! the connector checks the request's shape first, and refuses every path that is absolute,
//! has a `.`, `..` or empty component, or holds NUL before any CLI runs, so such a request
//! never reaches the Mac's store.
//!
//! `state.watch {since, wait_ms}` is `riwork watch --json` with the request on standard input:
//! it answers `{changed, revs}` as soon as a topic the device follows changed, or
//! `{unchanged:true, revs}` after `wait_ms`. A wait runs in the long-poll lane.
use super::*;

/// Allowlist v1: each method and whether it writes. The CLI's table
/// (`riwork_nsh_native::invoke::METHODS`) and the phone's (`NativeInvokeRequests` in
/// `ios/Core/RelayClient.swift`) are copies.
pub const METHODS: &[(&str, bool)] = &[
    ("Store.snapshot", false),
    ("Store.search", false),
    ("Store.renameProject", true),
    ("Store.updateProjectMetadata", true),
    ("Store.setProjectMemo", true),
    ("Store.setProjectNotifications", true),
    ("Store.setProjectOrchestratorWakes", true),
    ("Store.setProjectOrchestrator", true),
    ("Store.setProjectDelegationPolicy", true),
    ("Store.createFolder", true),
    ("Store.renameFolder", true),
    ("Store.moveFolder", true),
    ("Store.removeFolder", true),
    ("Store.moveProjectToFolder", true),
    ("Store.addTask", true),
    ("Store.assignTasks", true),
    ("Store.unassignTasks", true),
    ("Store.setTaskStatus", true),
    ("Store.refreshWorktrees", true),
    ("Store.syncWorktrees", true),
    ("Sessions.list", false),
    ("Sessions.createSubshell", true),
    ("Sessions.openEditor", true),
    ("Tabs.snapshot", false),
    ("Settings.get", false),
    ("Settings.set", true),
    ("Files.listDirectory", false),
    ("Files.preview", false),
    ("Files.readText", false),
    ("Schedules.list", false),
    ("Schedules.pause", true),
    ("Schedules.delete", true),
    ("Orchestrators.plan", false),
];

/// The topics `state.watch` takes, as the CLI's `riwork_nsh_native::state_watch::TOPICS`.
pub const WATCH_TOPICS: [&str; 7] = [
    "store",
    "sessions",
    "tabs",
    "settings",
    "schedules",
    "usage",
    "dashboard",
];
/// The longest `state.watch` wait, as for `chat.events`.
pub const WATCH_MAX_MS: u64 = 25_000;
/// The most projects whose tabs one `state.watch` follows.
pub const WATCH_MAX_TAB_PROJECTS: usize = 16;
/// How long a CLI that named no `native` methods is believed before it is asked again (an
/// updated desktop binary is then noticed without restarting the connector).
const NATIVE_RECHECK: Duration = Duration::from_secs(600);

/// Whether `module.method` writes: `Some(true)` for the ordered lane, `Some(false)` for a
/// read, `None` for a method that does not exist here.
pub fn is_write(module: &str, method: &str) -> Option<bool> {
    METHODS.iter().find_map(|(name, write)| {
        let (m, f) = name.split_once('.')?;
        (m == module && f == method).then_some(*write)
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Invoke {
    module: String,
    method: String,
    args: Value,
}

impl Invoke {
    fn name(&self) -> String {
        format!("{}.{}", self.module, self.method)
    }
}

/// A path a device names inside a project: relative, every component a plain name, no NUL.
/// The Mac checks it again against the project's root, without following links.
fn relative_path(value: Option<&Value>, root_allowed: bool) -> std::result::Result<(), Fault> {
    let Some(Value::String(path)) = value else {
        return Err(invalid("path must be a string relative to the project"));
    };
    if path.is_empty() {
        return if root_allowed {
            Ok(())
        } else {
            Err(invalid("path must name a file inside the project"))
        };
    }
    if path.len() > 4096
        || path.contains('\0')
        || path.starts_with('/')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid(
            "path must be relative to the project, with no '.', '..' or empty component",
        ));
    }
    Ok(())
}

pub(super) fn invoke_spec(r: &Request) -> std::result::Result<Invoke, Fault> {
    if !r.params.is_object() {
        return Err(invalid("params must be an object"));
    }
    let p: Invoke = params(r)?;
    if is_write(&p.module, &p.method).is_none() {
        return Err(invalid("unsupported native method"));
    }
    let Value::Object(args) = &p.args else {
        return Err(invalid("args must be an object"));
    };
    match p.name().as_str() {
        "Files.listDirectory" => relative_path(args.get("path"), true)?,
        "Files.preview" | "Files.readText" => relative_path(args.get("path"), false)?,
        "Sessions.openEditor" => relative_path(
            args.get("request").and_then(|request| request.get("path")),
            false,
        )?,
        _ => {}
    }
    Ok(p)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Watch {
    since: serde_json::Map<String, Value>,
    wait_ms: u64,
}

fn revision_shaped(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(rev) => hash_shaped(rev),
        _ => false,
    }
}

pub(super) fn watch_spec(r: &Request) -> std::result::Result<Watch, Fault> {
    if !r.params.is_object() {
        return Err(invalid("params must be an object"));
    }
    let p: Watch = params(r)?;
    if p.wait_ms > WATCH_MAX_MS {
        return Err(invalid(format!("wait_ms must be 0 to {WATCH_MAX_MS}")));
    }
    if p.since.is_empty() {
        return Err(invalid("since must name at least one topic"));
    }
    for (topic, rev) in &p.since {
        if !WATCH_TOPICS.contains(&topic.as_str()) {
            return Err(invalid(format!("unknown topic {topic:?}")));
        }
        if topic == "tabs" {
            let Value::Object(projects) = rev else {
                return Err(invalid("since.tabs maps project ids to revisions"));
            };
            if projects.is_empty() || projects.len() > WATCH_MAX_TAB_PROJECTS {
                return Err(invalid(format!(
                    "since.tabs names 1 to {WATCH_MAX_TAB_PROJECTS} projects"
                )));
            }
            for (project, rev) in projects {
                id(project)?;
                if !revision_shaped(rev) {
                    return Err(invalid("a revision is null or a short printable string"));
                }
            }
        } else if !revision_shaped(rev) {
            return Err(invalid("a revision is null or a short printable string"));
        }
    }
    Ok(p)
}

/// What the installed CLI said it has: `native.methods` and `watch.topics` of
/// `riwork capabilities --json`, each kept to what this connector also has.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NativeCapabilities {
    pub methods: Vec<&'static str>,
    pub topics: Vec<&'static str>,
}

impl NativeCapabilities {
    /// From the CLI's answer to `capabilities --json`; empty when it names neither.
    pub(super) fn from_reply(reply: &Value) -> Self {
        let named = |object: &str, list: &str| -> BTreeSet<String> {
            let Some(value) = reply.get(object).filter(|v| v.get("v") == Some(&json!(1))) else {
                return BTreeSet::new();
            };
            value
                .get(list)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        };
        let methods = named("native", "methods");
        let topics = named("watch", "topics");
        Self {
            methods: METHODS
                .iter()
                .map(|(name, _)| *name)
                .filter(|name| methods.contains(*name))
                .collect(),
            topics: WATCH_TOPICS
                .iter()
                .copied()
                .filter(|topic| topics.contains(*topic))
                .collect(),
        }
    }
}

/// The desktop CLI's error codes that pass to the device; anything else is `cli_error`.
fn fault_code(code: &str) -> &'static str {
    match code {
        "invalid_request" => "invalid_request",
        "not_found" => "not_found",
        "conflict" => "conflict",
        "refused" => "refused",
        "response_too_large" => "response_too_large",
        _ => "cli_error",
    }
}

/// `{"v":1,"error":{"code","message"}}` from the CLI as a fault, if it is one.
fn cli_refusal(cli: &Value) -> Option<Fault> {
    let error = cli.get("error")?;
    let code = fault_code(error.get("code").and_then(Value::as_str).unwrap_or(""));
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("the desktop refused the call");
    Some(Fault::new(
        code,
        message.chars().take(1000).collect::<String>(),
    ))
}

/// What the device is sent of `riwork native invoke`'s answer.
fn invoke_reply(cli: Value) -> std::result::Result<Value, Fault> {
    if let Some(fault) = cli_refusal(&cli) {
        return Err(fault);
    }
    if cli.get("v") != Some(&json!(1)) {
        return Err(cli_fault(
            "CLI returned a native answer of a version other than 1",
        ));
    }
    let Some(value) = cli.get("value") else {
        return Err(cli_fault("CLI returned a native answer without a value"));
    };
    let mut out = json!({"value": value});
    if let Some(page) = cli.get("page") {
        let count = |key: &str| page.get(key).and_then(Value::as_u64).is_some();
        if !(count("offset") && count("limit") && count("total")) {
            return Err(cli_fault("CLI returned a malformed page"));
        }
        out["page"] =
            json!({"offset": page["offset"], "limit": page["limit"], "total": page["total"]});
    }
    Ok(out)
}

/// What the device is sent of `riwork watch`'s answer, for the request `asked`.
fn watch_reply(cli: Value, asked: &Watch) -> std::result::Result<Value, Fault> {
    if let Some(fault) = cli_refusal(&cli) {
        return Err(fault);
    }
    if cli.get("v") != Some(&json!(1)) {
        return Err(cli_fault(
            "CLI returned a watch answer of a version other than 1",
        ));
    }
    let Some(Value::Object(revs)) = cli.get("revs") else {
        return Err(cli_fault("CLI returned a watch answer without revisions"));
    };
    let mut kept = serde_json::Map::new();
    for (topic, rev) in revs {
        if !asked.since.contains_key(topic) {
            continue;
        }
        let shaped = if topic == "tabs" {
            rev.as_object().is_some_and(|projects| {
                projects.iter().all(|(project, rev)| {
                    uuid(project).is_ok() && rev.as_str().is_some_and(hash_shaped)
                })
            })
        } else {
            rev.as_str().is_some_and(hash_shaped)
        };
        if !shaped {
            return Err(cli_fault("CLI returned a malformed revision"));
        }
        kept.insert(topic.clone(), rev.clone());
    }
    if cli.get("unchanged") == Some(&Value::Bool(true)) {
        return Ok(json!({"unchanged": true, "revs": kept}));
    }
    let Some(Value::Array(changed)) = cli.get("changed") else {
        return Err(cli_fault(
            "CLI returned a watch answer that is neither changed nor unchanged",
        ));
    };
    let changed = changed
        .iter()
        .filter_map(Value::as_str)
        .filter(|topic| asked.since.contains_key(*topic))
        .map(|topic| json!(topic))
        .collect::<Vec<_>>();
    if changed.is_empty() {
        return Err(cli_fault(
            "CLI returned a change of no topic that was asked about",
        ));
    }
    Ok(json!({"changed": changed, "revs": kept}))
}

impl Rpc {
    /// What `ready` announces as `features.native` and `features.watch`, from the same
    /// question as `features.chat`; `None` for a CLI that has neither (the standalone CLI, or a
    /// desktop from before them).
    pub async fn native_features(&self) -> (Option<Value>, Option<Value>) {
        let caps = self
            .native_known(Duration::from_secs(3))
            .await
            .unwrap_or_default();
        (
            (!caps.methods.is_empty()).then(|| json!({"v": 1, "methods": caps.methods})),
            (!caps.topics.is_empty()).then(|| json!({"v": 1, "topics": caps.topics})),
        )
    }

    /// What the CLI has of these. A CLI that named none is believed for `NATIVE_RECHECK`: every
    /// handshake asks about the other features only while one of them is still a no, and
    /// this one must not make it ask each time for a desktop binary that has none.
    async fn native_known(
        &self,
        limit: Duration,
    ) -> std::result::Result<NativeCapabilities, Fault> {
        let recent_no = self
            .native_no
            .lock()
            .expect("native no")
            .is_some_and(|at| at.elapsed() < NATIVE_RECHECK);
        if !self.native.load(Ordering::Relaxed) && !recent_no {
            if !self.capability_known(&self.native, limit).await? {
                *self.native_no.lock().expect("native no") = Some(std::time::Instant::now());
            }
        }
        Ok(self
            .native_capabilities
            .lock()
            .expect("native capabilities")
            .clone())
    }

    /// `module.method` with `args`, as `riwork native invoke` answers it. A write may have
    /// waited in the ordered lane: a device revoked meanwhile changes nothing. Once a write's
    /// CLI has started, it runs to the end even when the connection is torn down.
    pub(super) async fn native_invoke(
        &self,
        device: &str,
        p: Invoke,
        reply_limit: usize,
    ) -> std::result::Result<Value, Fault> {
        let name = p.name();
        let write = is_write(&p.module, &p.method).unwrap_or(true);
        if !self
            .native_known(CLI_TIMEOUT)
            .await?
            .methods
            .contains(&name.as_str())
        {
            return Err(invalid("unsupported RPC method"));
        }
        if write {
            self.still_authorized(device)?;
        }
        let args = serde_json::to_vec(&p.args).map_err(invalid)?;
        let command = vec!["native".into(), "invoke".into(), name, "--json".into()];
        let cap = reply_limit + 1024;
        let data = if write {
            // A write may scan a repository or start Vim, so it gets a creation's time. And
            // `Sessions.createSubshell` and `Sessions.openEditor` start a tmux session and only
            // then write it into the registry: a CLI killed in between leaves a session nobody
            // can see or close. The connection's tasks are dropped (and their CLI processes
            // killed) when it ends for any reason, revocation and relay errors included, so a
            // write's CLI runs in a task that outlives the request, as `Rpc::create`'s does: if
            // the request is dropped, only the answer is lost.
            let runner = self.detached();
            tokio::spawn(async move {
                runner
                    .raw_fed(command, Some(&args), CREATE_TIMEOUT, cap)
                    .await
            })
            .await
            .map_err(|e| cli_fault(format!("the desktop call was interrupted: {e}")))?
        } else {
            self.raw_fed(command, Some(&args), CLI_TIMEOUT, cap).await
        }
        .map_err(|e| {
            if e.is::<OutputTooLarge>() {
                Fault::new("response_too_large", "Too large to show on the phone")
            } else {
                cli_fault(e)
            }
        })?;
        invoke_reply(serde_json::from_slice(&data).map_err(cli_fault)?)
    }

    /// Waits as `riwork watch` does for one of the topics of `p` to change.
    pub(super) async fn state_watch(&self, p: Watch) -> std::result::Result<Value, Fault> {
        let caps = self.native_known(CLI_TIMEOUT).await?;
        if caps.topics.is_empty() {
            return Err(invalid("unsupported RPC method"));
        }
        if let Some(topic) = p
            .since
            .keys()
            .find(|topic| !caps.topics.contains(&topic.as_str()))
        {
            return Err(invalid(format!("the desktop does not watch {topic:?}")));
        }
        let request = serde_json::to_vec(&json!({"since": p.since, "wait_ms": p.wait_ms}))
            .map_err(invalid)?;
        let limit = CLI_TIMEOUT.max(Duration::from_millis(p.wait_ms) + CLI_WAIT_MARGIN);
        let data = self
            .raw_fed(
                vec!["watch".into(), "--json".into()],
                Some(&request),
                limit,
                MAX_PLAINTEXT,
            )
            .await
            .map_err(cli_fault)?;
        watch_reply(serde_json::from_slice(&data).map_err(cli_fault)?, &p)
    }
}
