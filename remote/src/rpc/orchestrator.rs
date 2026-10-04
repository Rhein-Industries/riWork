//! `orchestrator.create` ("Orchestrator creation extension" in `docs/remote-protocol.md`):
//! make the global orchestrator, or a project's, or get the one that is already there.
//!
//! It is `riwork orchestrator create [--project ID] --json`. Whether the orchestrator runs as
//! a terminal or as a chat, and with which provider, is the desktop's own setting ("Orchestrator
//! runs as") unless the phone supplies `mode` (chat or terminal). A scope that
//! already has an orchestrator, in either mode, gets that one back instead of a second.
use super::*;

/// Validated creation parameters. Omitting mode preserves the desktop setting.
pub(super) struct Spec {
    project: Option<String>,
    mode: Option<String>,
}

pub(super) fn spec(params: &Value) -> std::result::Result<Spec, Fault> {
    let object = params
        .as_object()
        .ok_or_else(|| invalid("params must be an object"))?;
    if let Some(unknown) = object
        .keys()
        .find(|k| !["project_id", "mode"].contains(&k.as_str()))
    {
        return Err(invalid(format!("unknown field {unknown}")));
    }
    let project = match object.get("project_id") {
        None => None,
        Some(Value::String(project)) => {
            id(project)?;
            Some(project.clone())
        }
        Some(_) => return Err(invalid("project_id must be a string")),
    };
    let mode = match object.get("mode") {
        None => None,
        Some(Value::String(mode)) if mode == "chat" || mode == "terminal" => Some(mode.clone()),
        Some(_) => return Err(invalid("mode must be terminal or chat")),
    };
    Ok(Spec { project, mode })
}

fn args(spec: &Spec) -> Vec<String> {
    let mut args: Vec<String> = vec!["orchestrator".into(), "create".into()];
    if let Some(project) = &spec.project {
        args.extend(["--project".into(), project.clone()]);
    }
    if let Some(mode) = &spec.mode {
        args.extend(["--mode".into(), mode.clone()]);
    }
    args
}

/// What a failed `riwork orchestrator create` says: the words of a failed `shell create`
/// (an unknown project, an agent that is not installed, a folder that is gone), except for
/// the connector's own timeout, which must not speak of a terminal: the orchestrator may be
/// a chat.
fn fault(fault: Fault) -> Fault {
    if fault.code == "cli_error" && fault.message == "RiWork CLI timeout" {
        return Fault::new(
            "cli_error",
            "creating the orchestrator took too long and was stopped; check the orchestrator list before trying again",
        );
    }
    create_fault(fault)
}

/// The `orchestrator.create` result for what the CLI printed, or why it is not usable: an
/// entry as `orchestrator list` prints it, of the scope that was asked for, and a `created`
/// that is a boolean, which is never guessed.
fn result(project: Option<&str>, cli: &Value) -> std::result::Result<Value, Fault> {
    let Some(created) = cli.get("created").and_then(Value::as_bool) else {
        return Err(cli_fault(
            "CLI did not say whether the orchestrator was created",
        ));
    };
    let text = |name: &str| cli.get(name).and_then(Value::as_str);
    let ours = text("id").is_some_and(|orchestrator| id(orchestrator).is_ok())
        && text("kind") == Some("orchestrator")
        && text("project_id") == project
        && text("cwd").is_some()
        && cli.get("alive").is_some_and(Value::is_boolean)
        && cli.get("created_at_unix").is_some_and(Value::is_u64);
    if !ours {
        return Err(cli_fault(
            "CLI returned an orchestrator that does not match the request",
        ));
    }
    // `created` is not part of the entry: it is about this call, not about the session.
    Ok(json!({"orchestrator": session_fields(cli.clone()), "created": created}))
}

impl Rpc {
    /// Whether the installed CLI can create orchestrators (`riwork capabilities --json` has
    /// `"orchestrator_create":true`): see `chat_known`, which it shares its question with.
    async fn orchestrator_create_known(&self, limit: Duration) -> std::result::Result<bool, Fault> {
        self.capability_known(&self.orchestrator_create, limit)
            .await
    }
    /// What `ready` announces as `features.orchestrator_create`. Short, like
    /// `chat_supported`: a CLI that does not answer within a few seconds does not delay the
    /// handshake any longer.
    pub async fn orchestrator_create_supported(&self) -> bool {
        self.orchestrator_create_known(Duration::from_secs(3))
            .await
            .unwrap_or(false)
    }
    async fn require_orchestrator_create(&self) -> std::result::Result<(), Fault> {
        if self.orchestrator_create_known(CLI_TIMEOUT).await? {
            Ok(())
        } else {
            Err(Fault::new(
                "cli_error",
                "the installed riwork CLI cannot create orchestrators from the phone; update RiWork",
            ))
        }
    }

    /// Make the orchestrator of the global scope, or of the project (which must exist under
    /// exactly this id), as `riwork orchestrator create` does, or answer with the one that
    /// exists. Runs in the ordered lane, which a phone that drops does not cut short, and the
    /// CLI itself runs in a task of its own (see below). A phone does not retry it by
    /// itself, as with `shell.create`; if it does, the repeat finds the first (`created`
    /// false) instead of making another.
    pub(super) async fn orchestrator_create(
        &self,
        device: &str,
        spec: Spec,
    ) -> std::result::Result<Value, Fault> {
        let project = &spec.project;
        self.require_orchestrator_create().await?;
        if let Some(project) = project {
            self.target_exists(&CreateTarget::Project(project.clone()), create_fault)
                .await?;
        }
        // Authorization was checked when the request started; this acts.
        self.still_authorized(device)?;
        // The CLI makes the session (a tmux session, or a chat and its agent) and only then
        // writes it down; a CLI killed in between leaves one nobody was told about. The
        // connection's tasks are dropped (and their CLI processes killed) when it ends for any
        // reason, so the CLI runs in a task that outlives the request: if the request is
        // dropped, only the answer is lost.
        let runner = self.detached();
        let argv = args(&spec);
        let created = tokio::spawn(async move { runner.read_within(argv, CREATE_TIMEOUT).await })
            .await
            .map_err(|e| cli_fault(format!("creating the orchestrator was interrupted: {e}")))?
            .map_err(fault)?;
        result(project.as_deref(), &created)
    }
}
