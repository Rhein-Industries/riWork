//! Worker cards join exact transcript markers with process/provider lifecycle reports.
use std::{collections::BTreeMap, path::Path};
use nsh_core::prelude::*;
use serde::{Serialize, Deserialize};
use riwork_engine::{activity, sessions::SessionManager, subshells, chat::{client::Client, model::Transcript, subagents, workers::{self, CardStatus, LiveStatus, WorkerKind}}};
use super::service::ChatBackend;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ChatWorker {
    pub id: String,
    pub item_id: String,
    pub session_id: Option<String>,
    pub kind: String,
    pub title: String,
    pub assignment: String,
    pub status: String,
    pub active: bool,
    pub output: String,
    pub detail: Option<String>,
    pub model: Option<String>,
    pub can_message: bool,
    pub can_stop: bool,
}

pub fn reported(transcript: &Transcript) -> Vec<ChatWorker> {
    let provider = transcript.info.as_ref().map(|info| info.provider);
    let now = activity::unix_now();
    let mut result = BTreeMap::new();
    for item in &transcript.items {
        if let Some(worker) = workers::of(item) {
            let status = CardStatus::of(worker, false, None, now);
            let id = format!("worker:{}", worker.session_id);
            result.insert(id.clone(), ChatWorker {
                id, item_id: item.id.clone(), session_id: Some(worker.session_id.clone()),
                kind: match worker.kind { WorkerKind::Shell => "shell", WorkerKind::Chat => "chat", _ => "unknown" }.into(),
                title: if worker.title.is_empty() { "Worker".into() } else { worker.title.clone() },
                assignment: worker.assignment.clone().unwrap_or_default(), status: status.label().into(), active: status.running(),
                output: worker.end.as_ref().map(|end| end.tail.clone()).unwrap_or_default(), detail: None,
                model: worker.model.clone(), can_message: false, can_stop: false,
            });
        }
    }
    // A wait may finish after a later send. Apply reports in replacement order,
    // preserving the exact child id and the last explicitly reported output.
    let mut reports: Vec<_> = transcript.items.iter().filter_map(|item| {
        subagents::of(item, provider).map(|run| (transcript.replaced_at(&item.id), item, run))
    }).collect();
    reports.sort_by_key(|(at, _, _)| *at);
    for (_, item, run) in reports {
        if run.children.is_empty() {
            let id = format!("subagent:{}", item.id);
            result.insert(id.clone(), ChatWorker {
                id, item_id: item.id.clone(), session_id: None, kind: "subagent".into(), title: run.name,
                assignment: run.purpose.unwrap_or_default(), status: run.status.label().into(), active: run.status == subagents::RunStatus::Running,
                output: run.summary.unwrap_or_default(), detail: None, model: None, can_message: false, can_stop: false,
            });
        } else {
            for child in run.children {
                let id = format!("subagent:{}", child.id);
                let previous: Option<ChatWorker> = result.remove(&id);
                result.insert(id.clone(), ChatWorker {
                    id, item_id: item.id.clone(), session_id: None, kind: "subagent".into(),
                    title: run.purpose.clone().filter(|text| !text.is_empty()).or_else(|| previous.as_ref().map(|row| row.title.clone())).unwrap_or_else(|| format!("Subagent · {}", child.id)),
                    assignment: run.purpose.clone().or_else(|| previous.as_ref().map(|row| row.assignment.clone())).unwrap_or_default(),
                    status: child.status.label().into(), active: child.status == subagents::RunStatus::Running,
                    output: child.summary.or_else(|| previous.map(|row| row.output)).unwrap_or_default(),
                    detail: None, model: None, can_message: false, can_stop: false,
                });
            }
        }
    }
    let mut rows: Vec<_> = result.into_values().collect();
    rows.sort_by_key(|row| (!row.active, row.id.clone()));
    rows
}

pub fn live(backend: &dyn ChatBackend, parent: &str, mut rows: Vec<ChatWorker>) -> Result<Vec<ChatWorker>, String> {
    let home = backend.home()?;
    if rows.iter().any(|row| row.kind == "shell") {
        match SessionManager::at(home.clone()).and_then(|manager| Ok((manager.list()?, manager))) {
            Ok((shells, manager)) => {
                let children: Vec<_> = shells.into_iter().filter(|shell| shell.parent_id.as_deref() == Some(parent)).collect();
                let states = activity::states_once(&home, &children, activity::unix_now());
                for row in rows.iter_mut().filter(|row| row.kind == "shell") {
                    let Some(shell) = children.iter().find(|shell| Some(&shell.id) == row.session_id.as_ref()) else { continue; };
                    let card = subshells::ShellCard::of(shell, states.get(&shell.id), &[]);
                    let status = CardStatus::Live(LiveStatus::of_shell(&card));
                    row.status = status.label().into(); row.active = status.running();
                    row.can_message = shell.alive; row.can_stop = shell.alive;
                    if shell.alive {
                        match manager.capture_sampled(shell, 16) {
                            Ok(output) => row.output = riwork_engine::chat::worker_summary::compress_output(&output),
                            Err(error) => row.detail = Some(format!("Output unavailable: {error}")),
                        }
                    }
                }
            }
            Err(error) => for row in rows.iter_mut().filter(|row| row.kind == "shell") { row.detail = Some(format!("Worker state unavailable: {error}")); },
        }
    }
    if rows.iter().any(|row| row.kind == "chat") {
        if let Ok(chats) = backend.existing().and_then(|socket| Client::connect(&socket)).and_then(|mut client| client.list()) {
            for row in rows.iter_mut().filter(|row| row.kind == "chat") {
                if let Some(chat) = chats.iter().find(|chat| Some(&chat.id) == row.session_id.as_ref() && chat.parent_id.as_deref() == Some(parent)) {
                    let status = CardStatus::Live(LiveStatus::of_chat(&chat.state));
                    row.status = status.label().into(); row.active = status.running(); row.model = chat.model.clone();
                }
            }
        }
    }
    rows.sort_by_key(|row| (!row.active, row.id.clone()));
    Ok(rows)
}

pub fn act(home: &Path, parent: &str, session: &str, action: &str, text: Option<&str>) -> Result<(), String> {
    let manager = SessionManager::at(home.to_owned())?;
    let shell = manager.list()?.into_iter().find(|shell| shell.id == session && shell.parent_id.as_deref() == Some(parent) && shell.alive)
        .ok_or("This worker is no longer running under this chat")?;
    match action {
        "send" => {
            let text = text.filter(|text| !text.trim().is_empty()).ok_or("Enter a message for the worker")?;
            manager.send(&shell.id, text)?;
            let _ = workers::send(home, parent, workers::WorkerNote::Assign { session_id: shell.id, kind: WorkerKind::Shell, text: text.into() });
            Ok(())
        }
        "stop" => manager.close(&shell.id),
        _ => Err("invalid: unknown worker action".into()),
    }
}
