//! `project.memo.get` and `project.memo.set` ("Project memo extension" in
//! `docs/remote-protocol.md`): a project's memo, the person's own Markdown notes for it, read and
//! replaced from the phone.
//!
//! They are `riwork project memo show|set ID --json`. The text of a save goes to the CLI on its
//! standard input, never as an argument (a process list shows arguments to every user of the
//! Mac). A save replaces the whole memo and the last one to reach the desktop's store wins:
//! there is no merge and no check against what the phone last saw. The connector checks the
//! parameters before any CLI runs, that the project exists under exactly the id given before a
//! save, and the CLI's reply before the phone sees it.
use super::*;

/// The most a memo may hold, in UTF-8 bytes: `riwork_core::project_memo::MAX_BYTES` (the
/// connector does not link the core; a test holds the two equal). The phone sends the text it
/// normalized, which the desktop stores unchanged, so a text this long is always one it keeps.
pub const MEMO_MAX_BYTES: usize = 32 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Get {
    project_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Set {
    project_id: String,
    text: String,
}

pub(super) fn get_spec(r: &Request) -> std::result::Result<Get, Fault> {
    if !r.params.is_object() {
        return Err(invalid("params must be an object"));
    }
    let p: Get = params(r)?;
    id(&p.project_id)?;
    Ok(p)
}

pub(super) fn set_spec(r: &Request) -> std::result::Result<Set, Fault> {
    if !r.params.is_object() {
        return Err(invalid("params must be an object"));
    }
    let p: Set = params(r)?;
    id(&p.project_id)?;
    if p.text.len() > MEMO_MAX_BYTES {
        return Err(invalid(format!(
            "text must be at most {MEMO_MAX_BYTES} bytes of UTF-8"
        )));
    }
    Ok(p)
}

/// What the phone is sent of the CLI's `project memo ... --json` for `project_id`, or why
/// nothing: `{"project_id","text","updated_at"?}`, or the CLI's own refusal by its code.
fn reply(cli: Value, project_id: &str) -> std::result::Result<Value, Fault> {
    if let Some(error) = cli.get("error") {
        let code = match error.get("code").and_then(Value::as_str) {
            Some("invalid_request") => "invalid_request",
            Some("not_found") => "not_found",
            _ => "cli_error",
        };
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the project memo could not be read or saved");
        return Err(Fault::new(code, message));
    }
    let bad = |what: &str| cli_fault(format!("CLI returned a project memo {what}"));
    if cli.get("v") != Some(&json!(1)) {
        return Err(bad("of a version other than 1"));
    }
    if cli.get("project_id").and_then(Value::as_str) != Some(project_id) {
        return Err(bad("of another project"));
    }
    let Some(text) = cli.get("text").and_then(Value::as_str) else {
        return Err(bad("without text"));
    };
    if text.len() > MEMO_MAX_BYTES {
        return Err(bad("longer than a memo may be"));
    }
    let mut out = json!({"project_id": project_id, "text": text});
    match cli.get("updated_at") {
        None => {}
        Some(at) if at.as_u64().is_some_and(|at| at > 0) => out["updated_at"] = at.clone(),
        Some(_) => return Err(bad("with an invalid updated_at")),
    }
    Ok(out)
}

/// `project show`'s refusal of an id: no project, or more than one, has it.
fn target_fault(fault: Fault) -> Fault {
    let line = fault
        .message
        .strip_prefix("RiWork CLI failed: riwork: ")
        .unwrap_or_default();
    if fault.code == "cli_error"
        && (line.starts_with("No project matches '")
            || line.starts_with("More than one project matches '"))
    {
        Fault::new("not_found", "project not found on the desktop")
    } else {
        fault
    }
}

impl Rpc {
    /// What `ready` announces as `features.project_memo`: the installed CLI said `"project_memo":
    /// true` in `riwork capabilities --json`. Short, like `chat_supported`, whose question it
    /// shares.
    pub async fn project_memo_supported(&self) -> bool {
        self.capability_known(&self.project_memo, Duration::from_secs(3))
            .await
            .unwrap_or(false)
    }

    async fn require_project_memo(&self) -> std::result::Result<(), Fault> {
        if self
            .capability_known(&self.project_memo, CLI_TIMEOUT)
            .await?
        {
            Ok(())
        } else {
            Err(invalid("unsupported RPC method"))
        }
    }

    /// The memo of the project, as `riwork project memo show ID --json` reads it.
    pub(super) async fn project_memo_get(&self, p: Get) -> std::result::Result<Value, Fault> {
        self.require_project_memo().await?;
        let cli = self
            .read_within(
                vec![
                    "project".into(),
                    "memo".into(),
                    "show".into(),
                    p.project_id.clone(),
                ],
                CLI_TIMEOUT,
            )
            .await?;
        reply(cli, &p.project_id)
    }

    /// Replace the memo of the project with `p.text`, through `riwork project memo set ID
    /// --json` with the text on its standard input, and answer the memo as saved. The project
    /// must exist under exactly this id: the CLI also matches names and paths, so an id that is
    /// nobody's could otherwise save over another project's memo.
    pub(super) async fn project_memo_set(
        &self,
        device: &str,
        p: Set,
    ) -> std::result::Result<Value, Fault> {
        self.require_project_memo().await?;
        // It may have waited in the ordered lane: a device revoked meanwhile saves nothing.
        self.still_authorized(device)?;
        self.target_exists(&CreateTarget::Project(p.project_id.clone()), target_fault)
            .await?;
        let args = vec![
            "project".into(),
            "memo".into(),
            "set".into(),
            p.project_id.clone(),
            "--json".into(),
        ];
        let data = self
            .raw_fed(args, Some(p.text.as_bytes()), CLI_TIMEOUT, MAX_PLAINTEXT)
            .await
            .map_err(cli_fault)?;
        let cli = serde_json::from_slice(&data).map_err(cli_fault)?;
        reply(cli, &p.project_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECT: &str = "00000000-0000-4000-8000-000000000002";

    fn request(method: &str, params: Value) -> Request {
        Request {
            v: 1,
            kind: "request".into(),
            id: "00000000-0000-4000-8000-000000000001".into(),
            method: method.into(),
            params,
        }
    }

    #[test]
    fn parameters_are_exact() {
        assert!(get_spec(&request("project.memo.get", json!({"project_id": PROJECT}))).is_ok());
        for params in [
            json!(null),
            json!([]),
            json!({}),
            json!({"project_id": "not-a-uuid"}),
            json!({"project_id": PROJECT, "text": ""}),
        ] {
            let fault = get_spec(&request("project.memo.get", params.clone()))
                .err()
                .unwrap_or_else(|| panic!("{params}"));
            assert_eq!(fault.code, "invalid_request", "{params}");
        }
        let full = "x".repeat(MEMO_MAX_BYTES);
        assert!(
            set_spec(&request(
                "project.memo.set",
                json!({"project_id": PROJECT, "text": full})
            ))
            .is_ok()
        );
        assert!(
            set_spec(&request(
                "project.memo.set",
                json!({"project_id": PROJECT, "text": ""})
            ))
            .is_ok()
        );
        for params in [
            json!({"project_id": PROJECT}),
            json!({"project_id": PROJECT, "text": null}),
            json!({"project_id": PROJECT, "text": 7}),
            json!({"project_id": PROJECT, "text": "x", "updated_at": 1}),
            json!({"project_id": PROJECT, "text": format!("{full}y")}),
            json!({"project_id": PROJECT, "text": "\u{e9}".repeat(MEMO_MAX_BYTES / 2 + 1)}),
        ] {
            let fault = set_spec(&request("project.memo.set", params.clone()))
                .err()
                .unwrap_or_else(|| panic!("{params}"));
            assert_eq!(fault.code, "invalid_request", "{params}");
        }
    }

    #[test]
    fn a_project_the_cli_does_not_find_by_its_id_is_not_found() {
        for line in [
            "No project matches 'x'",
            "More than one project matches 'x'; use its UUID",
        ] {
            let fault = target_fault(cli_fault(format!("RiWork CLI failed: riwork: {line}")));
            assert_eq!(fault.code, "not_found", "{line}");
        }
        let other = target_fault(cli_fault("RiWork CLI failed: riwork: Cannot lock store"));
        assert_eq!(other.code, "cli_error");
    }

    #[test]
    fn a_reply_is_the_memo_of_the_project_asked_about_or_the_clis_refusal() {
        let ok = reply(
            json!({"v": 1, "project_id": PROJECT, "text": "# Notes", "updated_at": 7, "extra": 1}),
            PROJECT,
        )
        .unwrap();
        assert_eq!(
            ok,
            json!({"project_id": PROJECT, "text": "# Notes", "updated_at": 7})
        );
        let never = reply(json!({"v": 1, "project_id": PROJECT, "text": ""}), PROJECT).unwrap();
        assert_eq!(never, json!({"project_id": PROJECT, "text": ""}));

        for (cli, code) in [
            (
                json!({"error": {"code": "not_found", "message": "No project"}}),
                "not_found",
            ),
            (
                json!({"error": {"code": "invalid_request", "message": "long"}}),
                "invalid_request",
            ),
            (
                json!({"error": {"code": "weird", "message": "x"}}),
                "cli_error",
            ),
            (
                json!({"v": 2, "project_id": PROJECT, "text": ""}),
                "cli_error",
            ),
            (
                json!({"v": 1, "project_id": "other", "text": ""}),
                "cli_error",
            ),
            (json!({"v": 1, "project_id": PROJECT}), "cli_error"),
            (
                json!({"v": 1, "project_id": PROJECT, "text": "x".repeat(MEMO_MAX_BYTES + 1)}),
                "cli_error",
            ),
            (
                json!({"v": 1, "project_id": PROJECT, "text": "", "updated_at": -1}),
                "cli_error",
            ),
            (
                json!({"v": 1, "project_id": PROJECT, "text": "", "updated_at": 0}),
                "cli_error",
            ),
        ] {
            assert_eq!(reply(cli.clone(), PROJECT).unwrap_err().code, code, "{cli}");
        }
    }
}
