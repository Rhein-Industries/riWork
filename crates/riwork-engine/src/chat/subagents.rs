//! Exact provider delegation kinds, never structure scraped from prose.
use super::model::{Item, ItemBody, ItemStatus, Provider, Transcript};
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
    /// Reported children past the bound, left out of `children`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: usize,
}
fn is_zero(value: &usize) -> bool {
    *value == 0
}
const CHILDREN: usize = 32;
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
        if self.omitted > 0 {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&format!(
                "{} more subagent{} not shown",
                self.omitted,
                if self.omitted == 1 { "" } else { "s" }
            ));
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
/// A field as the card shows it. A file it cites reads as its name, before the bound
/// can cut the citation.
fn text(value: &Value, key: &str, limit: usize) -> Option<String> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            super::citation::with_names(s)
                .chars()
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
    let mut children: Vec<_> = value["agentsStates"]
        .as_object()
        .into_iter()
        .flat_map(|states| states.iter())
        .map(|(id, state)| Child {
            id: id.chars().take(120).collect(),
            status: child_status(state),
            summary: text(state, "message", 4096),
        })
        .collect();
    // Past the bound a child of every reported status comes first, so the children left out
    // never change what the card says.
    let omitted = children.len().saturating_sub(CHILDREN);
    if omitted > 0 {
        let mut statuses = Vec::new();
        let (mut kept, rest): (Vec<_>, Vec<_>) = children.into_iter().partition(|child| {
            let first = !statuses.contains(&child.status);
            if first {
                statuses.push(child.status);
            }
            first
        });
        kept.extend(rest);
        kept.truncate(CHILDREN);
        children = kept;
    }
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
        omitted,
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
        omitted: 0,
    })
}
pub fn of(item: &Item, provider: Option<Provider>) -> Option<Subagent> {
    if let Some(run) = &item.presentation.subagent {
        let mut run = run.clone();
        // A run its item reported as going is over once the item is (its turn ended without a
        // report: the provider stopped or crashed). Children are reported for themselves.
        if run.status == RunStatus::Running
            && run.children.is_empty()
            && item.status != ItemStatus::InProgress
        {
            run.status = match item.status {
                ItemStatus::Completed => RunStatus::Unknown,
                status => item_status(status),
            };
        }
        return Some(run);
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
            omitted: 0,
        }),
        _ => None,
    }
}

/// A later wait/send/close report updates earlier cards for the same exact child id. Later is
/// when its item last changed, not where it sits: a wait that started before a send can finish
/// after it. A report without a message keeps the child's last result.
/// This does not infer activity between reports or treat operation completion as child completion.
pub fn in_transcript(
    item: &Item,
    transcript: &Transcript,
    provider: Option<Provider>,
) -> Option<Subagent> {
    let mut run = of(item, provider)?;
    if provider != Some(Provider::Codex) || run.children.is_empty() {
        return Some(run);
    }
    // When each child's status and its result were last reported.
    let mut reported_at = vec![(None, None); run.children.len()];
    for source in &transcript.items {
        let Some(reported) = of(source, provider) else {
            continue;
        };
        let at = Some(transcript.replaced_at(&source.id));
        for child in reported.children {
            let Some(ix) = run.children.iter().position(|ours| ours.id == child.id) else {
                continue;
            };
            let (status_at, summary_at) = &mut reported_at[ix];
            if at >= *status_at {
                *status_at = at;
                run.children[ix].status = child.status;
            }
            if child.summary.is_some() && at >= *summary_at {
                *summary_at = at;
                run.children[ix].summary = child.summary;
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
    use super::super::model::{ChatEvent, Transcript, TurnOutcome};
    use super::*;
    use serde_json::json;
    fn tool(name: &str, input: Value, status: ItemStatus) -> Item {
        Item {
            started_at: None,
            ended_at: None,
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
        // A file the child cites past the bound's reach still reads as its name.
        let cited = format!(
            "{}:codex-file-citation{{path=\"/out/{}/Bericht.pdf\"}}",
            "x".repeat(4090),
            "d".repeat(200)
        );
        value["agentsStates"]["child"]["message"] = json!(cited);
        let summary = codex(&value).children[0].summary.clone().unwrap();
        assert_eq!(summary, format!("{}Bericht.pdf", "x".repeat(4090))[..4096]);
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
        let mut transcript = Transcript::default();
        transcript.apply(&ChatEvent::ItemCompleted {
            item: spawn.clone(),
        });
        let mut wait = wait;
        wait.id = "wait".into();
        transcript.apply(&ChatEvent::ItemCompleted { item: wait });
        let run = in_transcript(&spawn, &transcript, Some(Provider::Codex)).unwrap();
        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(run.children.len(), 1);
        assert_eq!(run.children[0].summary.as_deref(), Some("Found"));
    }
    fn report(id: &str, operation: &str, states: Value, status: ItemStatus) -> ChatEvent {
        let mut item = tool(
            "collabAgentToolCall",
            json!({"tool":operation,"agentsStates":states}),
            status,
        );
        item.id = id.into();
        if status == ItemStatus::InProgress {
            ChatEvent::ItemStarted { item }
        } else {
            ChatEvent::ItemCompleted { item }
        }
    }
    fn card(transcript: &Transcript) -> Subagent {
        in_transcript(&transcript.items[0], transcript, Some(Provider::Codex)).unwrap()
    }
    #[test]
    fn the_newest_report_of_a_child_wins_wherever_its_item_sits() {
        let running = json!({"one":{"status":"running"}});
        let mut transcript = Transcript::default();
        transcript.apply(&report(
            "spawn",
            "spawnAgent",
            running.clone(),
            ItemStatus::Completed,
        ));
        // A wait that started before a send finishes after it.
        transcript.apply(&report("wait", "wait", json!({}), ItemStatus::InProgress));
        transcript.apply(&report(
            "send",
            "sendInput",
            running.clone(),
            ItemStatus::Completed,
        ));
        transcript.apply(&report(
            "wait",
            "wait",
            json!({"one":{"status":"completed","message":"Found"}}),
            ItemStatus::Completed,
        ));
        let run = card(&transcript);
        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(run.children[0].summary.as_deref(), Some("Found"));
        // A send after the wait finished really is newer: the child runs again.
        transcript.apply(&report(
            "again",
            "sendInput",
            running,
            ItemStatus::Completed,
        ));
        assert_eq!(card(&transcript).status, RunStatus::Running);
    }
    #[test]
    fn a_report_without_a_message_keeps_the_childs_result() {
        let mut transcript = Transcript::default();
        transcript.apply(&report(
            "spawn",
            "spawnAgent",
            json!({"one":{"status":"running"}}),
            ItemStatus::Completed,
        ));
        transcript.apply(&report(
            "wait",
            "wait",
            json!({"one":{"status":"completed","message":"Found"}}),
            ItemStatus::Completed,
        ));
        transcript.apply(&report(
            "close",
            "closeAgent",
            json!({"one":{"status":"shutdown"}}),
            ItemStatus::Completed,
        ));
        let run = card(&transcript);
        assert_eq!(run.status, RunStatus::Interrupted);
        assert_eq!(run.children[0].summary.as_deref(), Some("Found"));
    }
    #[test]
    fn children_beyond_the_bound_still_decide_the_status_and_are_counted() {
        let mut states = serde_json::Map::new();
        for n in 0..32 {
            states.insert(format!("a{n:02}"), json!({"status":"completed"}));
        }
        states.insert("z".into(), json!({"status":"running"}));
        let mut transcript = Transcript::default();
        transcript.apply(&report(
            "spawn",
            "spawnAgent",
            Value::Object(states),
            ItemStatus::Completed,
        ));
        let run = card(&transcript);
        assert_eq!(run.status, RunStatus::Running);
        assert_eq!(run.children.len(), 32);
        assert!(run.children.iter().any(|child| child.id == "z"));
        assert!(run.result_text().ends_with("1 more subagent not shown"));
    }
    #[test]
    fn a_muse_run_left_running_ends_with_its_turn() {
        let value = json!({"kind":"subagent","role":"Reviewer","status":"inProgress"});
        let mut item = tool("Reviewer", json!({}), ItemStatus::InProgress);
        item.turn_id = Some("turn".into());
        item.presentation.subagent = muse(&value, ItemStatus::InProgress);
        let mut transcript = Transcript::default();
        transcript.apply(&ChatEvent::ItemStarted { item });
        assert_eq!(
            of(&transcript.items[0], Some(Provider::Muse))
                .unwrap()
                .status,
            RunStatus::Running
        );
        transcript.apply(&ChatEvent::TurnCompleted {
            ended_at: None,
            turn_id: "turn".into(),
            outcome: TurnOutcome::Failed {
                message: "exited".into(),
            },
        });
        assert_eq!(
            of(&transcript.items[0], Some(Provider::Muse))
                .unwrap()
                .status,
            RunStatus::Failed
        );
    }
}
