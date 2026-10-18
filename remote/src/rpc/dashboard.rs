//! `dashboard.get` ("Dashboard extension" in `docs/remote-protocol.md`): the desktop's global
//! dashboard in one bounded read, as the desktop's own dashboard shows it.
//!
//! It is `riwork dashboard --json`, which samples what the desktop's dashboard samples (the
//! store, the shells and their agents, the chats of a chat host that runs or else the saved
//! chats, and the automations) and starts nothing. The CLI already arranges, orders and cuts
//! the reply (`riwork_engine::global_overview::wire`); the connector only checks it before the
//! phone sees it: version 1, the contract's top-level keys and nothing else, each list an array
//! of objects no longer than its limit. The reply limit of the session applies as to any read.
use super::*;

/// The top-level keys of a reply. Anything else the CLI writes is left out.
pub const DASHBOARD_KEYS: [&str; 23] = [
    "v",
    "generated_at",
    "chats_known",
    "totals",
    "global",
    "attention",
    "attention_total",
    "working",
    "working_total",
    "finished",
    "finished_total",
    "projects",
    "projects_total",
    "tasks_in_progress",
    "tasks_in_progress_total",
    "automations",
    "backends",
    "decisions",
    "decisions_total",
    "decision_sources",
    "recent_activity",
    "recent_activity_total",
    "intelligence",
];
/// Each list of a reply and the most rows the phone is sent of it, the CLI's own limits
/// (`riwork_engine::global_overview::WIRE_MAX_*`). A CLI that wrote more has the tail cut;
/// `<list>_total` still says how many there are. A dotted name addresses a nested list.
pub const DASHBOARD_LISTS: [(&str, usize); 8] = [
    ("attention", 40),
    ("working", 40),
    ("finished", 20),
    ("projects", 150),
    ("tasks_in_progress", 50),
    ("backends.providers", 8),
    ("decisions", 40),
    ("recent_activity", 40),
];
/// The most `automations.items` the phone is sent.
pub const DASHBOARD_AUTOMATIONS_MAX: usize = 30;

/// `rows`, a list of the reply, as the phone may see it: its objects, at most `cap` of them.
fn rows(rows: &mut Value, cap: usize, name: &str) -> std::result::Result<(), Fault> {
    let Value::Array(items) = rows else {
        return Err(cli_fault(format!(
            "CLI returned a dashboard whose {name} is not a list"
        )));
    };
    items.retain(Value::is_object);
    items.truncate(cap);
    Ok(())
}

/// What the phone is sent of the CLI's `dashboard --json`, or why nothing.
fn reply(cli: Value) -> std::result::Result<Value, Fault> {
    let Value::Object(mut reply) = cli else {
        return Err(cli_fault("CLI returned a dashboard that is not an object"));
    };
    if reply.get("v") != Some(&json!(1)) {
        return Err(cli_fault(
            "CLI returned a dashboard of a version other than 1",
        ));
    }
    reply.retain(|key, _| DASHBOARD_KEYS.contains(&key.as_str()));
    for (list, cap) in DASHBOARD_LISTS {
        let value = match list.split_once('.') {
            Some((object, field)) => reply.get_mut(object).and_then(|value| value.get_mut(field)),
            None => reply.get_mut(list),
        };
        if let Some(value) = value {
            rows(value, cap, list)?;
        }
    }
    if let Some(Value::Object(automations)) = reply.get_mut("automations")
        && let Some(items) = automations.get_mut("items")
    {
        rows(items, DASHBOARD_AUTOMATIONS_MAX, "automations.items")?;
    }
    Ok(Value::Object(reply))
}

impl Rpc {
    /// What `ready` announces as `features.dashboard`: the installed CLI said `"dashboard":
    /// true` in `riwork capabilities --json`. Short, like `chat_supported`, whose question it
    /// shares.
    pub async fn dashboard_supported(&self) -> bool {
        self.capability_known(&self.dashboard, Duration::from_secs(3))
            .await
            .unwrap_or(false)
    }

    /// The global dashboard, as `riwork dashboard --json` reads it. Without that command the
    /// method is one this desktop does not have, in the words a connector from before it uses.
    pub(super) async fn dashboard(&self, reply_limit: usize) -> std::result::Result<Value, Fault> {
        if !self.capability_known(&self.dashboard, CLI_TIMEOUT).await? {
            return Err(invalid("unsupported RPC method"));
        }
        // As big as a reply may be for this session; the CLI writes one line of JSON.
        let cli = self
            .read_capped(vec!["dashboard".into()], CLI_TIMEOUT, reply_limit)
            .await
            .map_err(|fault| {
                if fault.code == "response_too_large" {
                    Fault::new(
                        "response_too_large",
                        "the dashboard exceeds the encrypted response limit; turn on compression with link.configure",
                    )
                } else {
                    fault
                }
            })?;
        reply(cli)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_keeps_the_contract_and_cuts_what_is_too_long() {
        let rows_of = |n: usize| (0..n).map(|i| json!({"n": i})).collect::<Vec<_>>();
        let mut cli = json!({
            "v": 1, "generated_at": 7, "chats_known": true, "totals": {}, "global": {"kind": "unknown"},
            "attention": rows_of(45), "attention_total": 45,
            "working": [1, "x", null, {"n": 0}], "working_total": 4,
            "finished": [], "finished_total": 0,
            "projects": rows_of(151), "projects_total": 151,
            "tasks_in_progress": rows_of(50), "tasks_in_progress_total": 50,
            "automations": {"state": "read", "items": rows_of(31), "items_total": 31, "extra": 1},
            "root": "/Users/me", "model": "secret"
        });
        let reply = reply(cli.clone()).unwrap();
        assert_eq!(reply["attention"].as_array().unwrap().len(), 40);
        assert_eq!(reply["attention"][39], json!({"n": 39}));
        assert_eq!(reply["attention_total"], 45);
        assert_eq!(reply["working"], json!([{"n": 0}]));
        assert_eq!(reply["projects"].as_array().unwrap().len(), 150);
        assert_eq!(reply["tasks_in_progress"].as_array().unwrap().len(), 50);
        assert_eq!(reply["automations"]["items"].as_array().unwrap().len(), 30);
        assert_eq!(reply["automations"]["items_total"], 31);
        assert!(reply.get("root").is_none() && reply.get("model").is_none());
        // Only the top level is the connector's to trim.
        assert_eq!(reply["automations"]["extra"], 1);

        cli["projects"] = json!({"0": {}});
        assert_eq!(reply_code(cli.clone()), "cli_error");
        for v in [json!(2), json!("1"), json!(1.0), Value::Null] {
            cli["v"] = v;
            assert_eq!(reply_code(cli.clone()), "cli_error");
        }
        assert_eq!(reply_code(json!([{"v": 1}])), "cli_error");
    }

    fn reply_code(cli: Value) -> &'static str {
        reply(cli).unwrap_err().code
    }
}
