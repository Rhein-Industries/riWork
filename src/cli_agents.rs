//! The agent fields of `riwork shell list --json`, `riwork orchestrator list
//! --json` and `riwork project list --json`, which the remote connector passes
//! on to the phone (`remote/src/rpc.rs`, `SESSION_FIELDS` and `PROJECT_FIELDS`).
//!
//! Activity is read cold in the CLI's own process by the code the desktop
//! window uses (`activity`, `agent_hooks`), so it needs no window and no
//! snapshot file. Recency is the one thing only the window knows: it reads the
//! file the window publishes (`recency_file`).

use std::{collections::BTreeMap, path::Path};

use serde::Serialize;

use crate::{
    activity::{AgentActivity, AgentState, states_once, unix_now},
    sessions::ShellSession,
    store::Project,
};

/// A shell as the JSON lists show it: everything `ShellSession` has, plus what
/// its agent is doing. Each addition is left out when unknown.
#[derive(Serialize)]
pub struct ShellEntry<'a> {
    #[serde(flatten)]
    shell: &'a ShellSession,
    /// `working`, `waiting`, `done`, `unknown` or `exited`. Absent for a shell
    /// that runs no agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    activity: Option<&'static str>,
    /// Unix seconds at which `activity` began, when the source records it.
    #[serde(skip_serializing_if = "Option::is_none")]
    activity_since_unix: Option<u64>,
    /// Subagents running under a working agent. Absent when there are none.
    #[serde(skip_serializing_if = "Option::is_none")]
    subagents_working: Option<usize>,
    /// Their kinds (`general-purpose`, `explorer`), at most four.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    subagent_kinds: Vec<String>,
}

pub fn shell_entries<'a>(home: &Path, shells: &'a [ShellSession]) -> Vec<ShellEntry<'a>> {
    let states = states_once(home, shells, unix_now());
    shells
        .iter()
        .map(|shell| match states.get(&shell.id) {
            Some(state) => ShellEntry {
                shell,
                activity: Some(state.activity.as_str()),
                activity_since_unix: state
                    .since_unix
                    .filter(|_| state.activity != AgentActivity::Exited),
                subagents_working: (state.subagents.working > 0).then_some(state.subagents.working),
                subagent_kinds: state.subagents.kinds.clone(),
            },
            None => ShellEntry {
                shell,
                activity: None,
                activity_since_unix: None,
                subagents_working: None,
                subagent_kinds: Vec::new(),
            },
        })
        .collect()
}

/// The agents of one project that are in each state. `waiting` and `done` are
/// the two ways an agent is idle: `waiting` has no finished turn to show (a
/// new session, or an interrupted turn), `done` has just finished one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AgentCounts {
    pub working: usize,
    pub waiting: usize,
    pub done: usize,
}

/// A project as `project list --json` shows it: the project, when the app last
/// saw one of its files change, and how many agents it has running.
#[derive(Serialize)]
pub struct ProjectEntry<'a> {
    #[serde(flatten)]
    project: &'a Project,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_edited_unix: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agents: Option<AgentCounts>,
}

/// `agents` is left out for every project when the shell list could not be
/// read (no tmux), so "none" is never reported for "could not tell".
pub fn project_entries<'a>(
    home: &Path,
    projects: &'a [Project],
    shells: Option<&[ShellSession]>,
) -> Vec<ProjectEntry<'a>> {
    let recency = crate::recency_file::read(home);
    let counts = shells.map(|shells| agent_counts(&states_once(home, shells, unix_now()), shells));
    projects
        .iter()
        .map(|project| ProjectEntry {
            project,
            last_edited_unix: recency
                .as_ref()
                .and_then(|published| published.projects.get(&project.id).copied()),
            agents: counts
                .as_ref()
                .map(|counts| counts.get(&project.id).copied().unwrap_or_default()),
        })
        .collect()
}

/// Counts per project id over every agent shell the project owns, its
/// orchestrator included, as the desktop's project rows do.
fn agent_counts(
    states: &BTreeMap<String, AgentState>,
    shells: &[ShellSession],
) -> BTreeMap<String, AgentCounts> {
    let mut counts: BTreeMap<String, AgentCounts> = BTreeMap::new();
    for shell in shells {
        let (Some(project), Some(state)) = (shell.project_id.as_ref(), states.get(&shell.id))
        else {
            continue;
        };
        let entry = counts.entry(project.clone()).or_default();
        match state.activity {
            AgentActivity::Working => entry.working += 1,
            AgentActivity::Waiting => entry.waiting += 1,
            AgentActivity::Done => entry.done += 1,
            AgentActivity::Unknown | AgentActivity::Exited => {}
        }
    }
    counts
}
