//! Tab operations never control session processes. Validation happens before CLI execution.
use super::*;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    project_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    project_id: String,
    key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    project_id: String,
    update: Update,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Update {
    Hide { key: String },
    Unhide { key: String },
    Move { key: String, before: Option<String> },
    Rename { key: String, title: String },
}
fn key(value: &str) -> std::result::Result<(), Fault> {
    let (kind, value) = value
        .split_once(':')
        .ok_or_else(|| invalid("invalid tab key"))?;
    if !matches!(kind, "chat" | "shell") {
        return Err(invalid("invalid tab kind"));
    }
    id(value)
}
fn arguments(r: &Request) -> std::result::Result<Vec<String>, Fault> {
    let mut args = vec!["tabs".into()];
    if r.method == "tabs.list" {
        let p: List = params(r)?;
        id(&p.project_id)?;
        args.extend(["list".into(), "--project".into(), p.project_id]);
    } else if r.method == "tabs.open" {
        let p: Open = params(r)?;
        id(&p.project_id)?;
        key(&p.key)?;
        args.extend([
            "open".into(),
            "--project".into(),
            p.project_id,
            "--key".into(),
            p.key,
        ]);
    } else {
        let p: Change = params(r)?;
        id(&p.project_id)?;
        match &p.update {
            Update::Hide { key: k } | Update::Unhide { key: k } => key(k)?,
            Update::Move { key: k, before } => {
                key(k)?;
                if let Some(before) = before {
                    key(before)?;
                }
            }
            Update::Rename { key: k, title } => {
                key(k)?;
                if title.chars().count() > 200
                    || title.chars().any(|c| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}'))
                {
                    return Err(invalid("title must be at most 200 printable characters"));
                }
            }
        }
        args.extend([
            "update".into(),
            "--project".into(),
            p.project_id,
            "--update-json".into(),
            serde_json::to_string(&p.update).map_err(invalid)?,
        ]);
    }
    Ok(args)
}
impl Rpc {
    /// Read the additive flag learned by the existing handshake capability probe.
    pub fn tabs_advertised(&self) -> bool {
        self.tabs.load(Ordering::Relaxed)
    }
    pub(super) async fn project_tabs(
        &self,
        device: &str,
        r: &Request,
    ) -> std::result::Result<Value, Fault> {
        let args = arguments(r)?;
        if !self.capability_known(&self.tabs, CLI_TIMEOUT).await? {
            return Err(invalid(
                "unsupported RPC method: shared project tabs unavailable",
            ));
        }
        self.still_authorized(device)?;
        let value = self.read_within(args, CLI_TIMEOUT).await?;
        if let Some(error) = value.get("error") {
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("cli_error");
            let code = match code {
                "invalid_request" => "invalid_request",
                "not_found" => "not_found",
                _ => "cli_error",
            };
            return Err(Fault::new(
                code,
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("tab operation failed"),
            ));
        }
        Ok(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
    fn tabs_list_arguments() {
        let args = arguments(&request(
            "tabs.list",
            json!({"project_id":"00000000-0000-4000-8000-000000000002"}),
        ))
        .unwrap();
        assert_eq!(&args[..2], &["tabs", "list"]);
        assert!(arguments(&request("tabs.list", json!({"project_id":"no"}))).is_err());
    }
    #[test]
    fn tabs_open_unhides_an_existing_session_without_process_commands() {
        let args = arguments(&request("tabs.open", json!({"project_id":"00000000-0000-4000-8000-000000000002","key":"shell:00000000-0000-4000-8000-000000000003"}))).unwrap();
        assert_eq!(&args[..2], &["tabs", "open"]);
        assert_eq!(args[4], "--key");
        assert!(
            arguments(&request(
                "tabs.open",
                json!({"project_id":"00000000-0000-4000-8000-000000000002","key":"invalid"})
            ))
            .is_err()
        );
    }
    #[test]
    fn tabs_update_arguments() {
        for action in ["hide", "unhide"] {
            let args=arguments(&request("tabs.update",json!({"project_id":"00000000-0000-4000-8000-000000000002","update":{"action":action,"key":"chat:00000000-0000-4000-8000-000000000003"}}))).unwrap();
            assert_eq!(args[4], "--update-json");
            assert!(args[5].contains(action));
        }
    }
    #[test]
    fn tabs_updates_refuse_unknown_and_invalid_fields() {
        for update in [
            json!({"action":"pin","key":"x"}),
            // Pins are gone: an older phone's Pin/Unpin is an invalid request.
            json!({"action":"pin","key":"chat:00000000-0000-4000-8000-000000000003"}),
            json!({"action":"unpin","key":"chat:00000000-0000-4000-8000-000000000003"}),
            json!({"action":"rename","key":"shell:00000000-0000-4000-8000-000000000003","title":"\n"}),
            json!({"action":"hide","key":"chat:00000000-0000-4000-8000-000000000003","stop":true}),
        ] {
            let fault = arguments(&request(
                "tabs.update",
                json!({"project_id":"00000000-0000-4000-8000-000000000002","update":update}),
            ))
            .unwrap_err();
            assert_eq!(fault.code, "invalid_request");
        }
    }
}
