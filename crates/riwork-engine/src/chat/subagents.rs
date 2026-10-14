//! Exact provider delegation kinds, never structure scraped from prose.
use super::model::{Item, ItemBody, ItemStatus, Provider};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Done,
    Failed,
    Interrupted,
    Declined,
    Unknown,
}
impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Interrupted => "Interrupted",
            Self::Declined => "Declined",
            Self::Unknown => "Status not reported",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Child {
    pub id: String,
    pub status: RunStatus,
    pub summary: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subagent {
    pub name: String,
    pub purpose: Option<String>,
    pub status: RunStatus,
    pub summary: Option<String>,
    #[serde(default)]
    pub children: Vec<Child>,
}
impl Subagent {
    pub fn result_text(&self) -> String {
        let mut result = self.summary.clone().unwrap_or_default();
        for child in &self.children {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&format!("{} · {}", child.id, child.status.label()));
            if let Some(summary) = &child.summary {
                result.push('\n');
                result.push_str(summary);
            }
        }
        if result.len() > 32 * 1024 {
            let mut end = 32 * 1024;
            while !result.is_char_boundary(end) {
                end -= 1;
            }
            result.truncate(end);
            result.push_str("\n… Summary truncated");
        }
        result
    }
}
fn text(value: &Value, key: &str, limit: usize) -> Option<String> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            s.chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .take(limit)
                .collect()
        })
}
fn item_status(status: ItemStatus) -> RunStatus {
    match status {
        ItemStatus::InProgress => RunStatus::Running,
        ItemStatus::Completed => RunStatus::Done,
        ItemStatus::Failed => RunStatus::Failed,
        ItemStatus::Interrupted => RunStatus::Interrupted,
        ItemStatus::Declined => RunStatus::Declined,
    }
}
fn child_status(value: &Value) -> RunStatus {
    match value["status"].as_str() {
        Some("running" | "pendingInit") => RunStatus::Running,
        Some("completed") => RunStatus::Done,
        Some("errored") => RunStatus::Failed,
        Some("shutdown" | "interrupted") => RunStatus::Interrupted,
        _ => RunStatus::Unknown,
    }
}
pub fn codex(value: &Value) -> Subagent {
    let children: Vec<_> = value["agentsStates"]
        .as_object()
        .into_iter()
        .flat_map(|states| states.iter())
        .take(32)
        .map(|(id, state)| Child {
            id: id.chars().take(120).collect(),
            status: child_status(state),
            summary: text(state, "message", 4096),
        })
        .collect();
    // A completed spawn/wait/send operation is not proof that its child completed.
    let status = if children.iter().any(|c| c.status == RunStatus::Running) {
        RunStatus::Running
    } else if children.iter().any(|c| c.status == RunStatus::Failed) {
        RunStatus::Failed
    } else if !children.is_empty() && children.iter().all(|c| c.status == RunStatus::Done) {
        RunStatus::Done
    } else {
        RunStatus::Unknown
    };
    Subagent {
        name: format!(
            "Subagents · {}",
            text(value, "tool", 80).unwrap_or_else(|| "delegation".into())
        ),
        purpose: text(value, "prompt", 500),
        status,
        summary: None,
        children,
    }
}
pub fn muse(value: &Value, status: ItemStatus) -> Option<Subagent> {
    (value["kind"] == "subagent").then(|| Subagent {
        name: text(value, "role", 120).unwrap_or_else(|| "Subagent".into()),
        purpose: text(value, "objective", 500),
        status: match value["status"].as_str() {
            Some("inProgress") => RunStatus::Running,
            Some("completed") => RunStatus::Done,
            Some("failed" | "timedOut") => RunStatus::Failed,
            Some("cancelled") => RunStatus::Interrupted,
            Some("rejected") => RunStatus::Declined,
            Some(_) => RunStatus::Unknown,
            None => item_status(status),
        },
        summary: text(&value["result"], "summary", 4096)
            .or_else(|| text(&value["result"], "text", 4096))
            .or_else(|| text(value, "text", 4096)),
        children: Vec::new(),
    })
}
pub fn of(item: &Item, provider: Option<Provider>) -> Option<Subagent> {
    if let Some(run) = &item.presentation.subagent {
        return Some(run.clone());
    }
    let ItemBody::ToolCall {
        server: None,
        tool,
        input,
        output,
    } = &item.body
    else {
        return None;
    };
    match (provider, tool.as_str()) {
        (Some(Provider::Codex), "collabAgentToolCall") => Some(codex(input)),
        (Some(Provider::Claude), "Task" | "Agent") => Some(Subagent {
            name: text(input, "subagent_type", 120).unwrap_or_else(|| "Subagent".into()),
            purpose: text(input, "description", 500).or_else(|| text(input, "prompt", 500)),
            status: if input["run_in_background"] == true && item.status == ItemStatus::Completed {
                RunStatus::Unknown
            } else {
                item_status(item.status)
            },
            summary: output.as_ref().map(|s| s.chars().take(4096).collect()),
            children: Vec::new(),
        }),
        _ => None,
    }
}

/// A later wait/send/close report updates earlier cards for the same exact child id.
/// This does not infer activity between reports or treat operation completion as child completion.
pub fn in_transcript(item: &Item, items: &[Item], provider: Option<Provider>) -> Option<Subagent> {
    let mut run = of(item, provider)?;
    if provider != Some(Provider::Codex) || run.children.is_empty() {
        return Some(run);
    }
    for source in items {
        if let Some(reported) = of(source, provider) {
            for child in reported.children {
                if let Some(ours) = run.children.iter_mut().find(|ours| ours.id == child.id) {
                    *ours = child;
                }
            }
        }
    }
    run.status = if run.children.iter().any(|c| c.status == RunStatus::Running) {
        RunStatus::Running
    } else if run.children.iter().any(|c| c.status == RunStatus::Failed) {
        RunStatus::Failed
    } else if run.children.iter().all(|c| c.status == RunStatus::Done) {
        RunStatus::Done
    } else if run
        .children
        .iter()
        .all(|c| c.status == RunStatus::Interrupted)
    {
        RunStatus::Interrupted
    } else {
        RunStatus::Unknown
    };
    Some(run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn tool(name: &str, input: Value, status: ItemStatus) -> Item {
        Item {
            id: "i".into(),
            turn_id: None,
            status,
            body: ItemBody::ToolCall {
                server: None,
                tool: name.into(),
                input,
                output: Some("Result".into()),
            },
            presentation: Default::default(),
        }
    }
    #[test]
    fn claude_does_not_claim_background_completion() {
        let mut item = tool(
            "Agent",
            json!({"subagent_type":"Explore","description":"Find the handler","run_in_background":true}),
            ItemStatus::InProgress,
        );
        assert_eq!(
            of(&item, Some(Provider::Claude)).unwrap().status,
            RunStatus::Running
        );
        item.status = ItemStatus::Completed;
        let card = of(&item, Some(Provider::Claude)).unwrap();
        assert_eq!(card.status, RunStatus::Unknown);
        assert_eq!(card.purpose.as_deref(), Some("Find the handler"));
        assert_eq!(card.summary.as_deref(), Some("Result"));
        assert!(of(&item, Some(Provider::Muse)).is_none());
        assert!(
            of(
                &tool(
                    "Read",
                    json!({"description":"subagent"}),
                    ItemStatus::Completed
                ),
                Some(Provider::Claude)
            )
            .is_none()
        );
    }
    #[test]
    fn codex_operations_and_child_status_are_distinct_and_bounded() {
        let mut value = json!({"tool":"spawnAgent","prompt":"inspect","status":"completed"});
        assert_eq!(codex(&value).status, RunStatus::Unknown);
        value["agentsStates"] = json!({"child":{"status":"running"}});
        assert_eq!(codex(&value).status, RunStatus::Running);
        value["agentsStates"]["child"] = json!({"status":"completed","message":"x".repeat(5000)});
        let run = codex(&value);
        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(run.children[0].summary.as_ref().unwrap().len(), 4096);
        value["agentsStates"]["child"]["status"] = json!("futureStatus");
        assert_eq!(codex(&value).status, RunStatus::Unknown);
    }
    #[test]
    fn muse_requires_its_explicit_subagent_kind() {
        assert!(
            muse(
                &json!({"kind":"toolCall","objective":"delegate"}),
                ItemStatus::Completed
            )
            .is_none()
        );
        let value = json!({"kind":"subagent","role":"Reviewer","objective":"Review","status":"completed","result":{"summary":"Approved"}});
        let run = muse(&value, ItemStatus::Completed).unwrap();
        assert_eq!(run.name, "Reviewer");
        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(run.summary.as_deref(), Some("Approved"));
        assert_eq!(
            muse(
                &json!({"kind":"subagent","status":"futureStatus"}),
                ItemStatus::Completed
            )
            .unwrap()
            .status,
            RunStatus::Unknown
        );
    }
    #[test]
    fn a_later_child_report_updates_the_spawn_card_by_exact_child_id() {
        let spawn = tool(
            "collabAgentToolCall",
            json!({"tool":"spawnAgent","agentsStates":{"one":{"status":"running"}}}),
            ItemStatus::Completed,
        );
        let wait = tool(
            "collabAgentToolCall",
            json!({"tool":"wait","agentsStates":{"one":{"status":"completed","message":"Found"},"other":{"status":"errored"}}}),
            ItemStatus::Completed,
        );
        let run = in_transcript(&spawn, &[spawn.clone(), wait], Some(Provider::Codex)).unwrap();
        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(run.children.len(), 1);
        assert_eq!(run.children[0].summary.as_deref(), Some("Found"));
    }
}
