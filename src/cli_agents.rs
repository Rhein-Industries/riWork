//! The agent fields of `riwork shell list --json`, `riwork orchestrator list
//! --json` and `riwork project list --json`, which the remote connector passes
//! on to the phone (`remote/src/rpc.rs`, `SESSION_FIELDS` and `PROJECT_FIELDS`).
//!
//! Activity is read cold in the CLI's own process by the code the desktop
//! window uses (`activity`, `agent_hooks`), so it needs no window and no
//! snapshot file. Shell activity times come from tmux together with the list
//! of live sessions (`SessionManager::list_with_activity`). File-edit recency
//! is the one thing only the window knows: it reads the file the window
//! publishes (`recency_file`).

use std::{collections::BTreeMap, path::Path};

use serde::Serialize;

use crate::{
    activity::{AgentActivity, AgentState, states_once, unix_now},
    orchestrators::ProjectFact,
    sessions::{SessionActivity, ShellSession},
    store::Project,
};

/// A shell as the JSON lists show it: everything `ShellSession` has, plus what
/// its agent is doing. Each addition is left out when unknown.
#[derive(Serialize)]
pub struct ShellEntry<'a> {
    #[serde(flatten)]
    shell: &'a ShellSession,
    /// `terminal`, on an orchestrator's entry, whose other kind runs as a chat
    /// (`orchestrators::chat_entry`). Absent on a shell, which is always one.
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'static str>,
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
    /// Unix seconds at which tmux last saw output in the shell's session: an
    /// agent working, text typed into it. Present for every live shell,
    /// whatever it runs; absent for one that is gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_activity_unix: Option<u64>,
}

pub fn shell_entries<'a>(
    home: &Path,
    shells: &'a [ShellSession],
    activity: &SessionActivity,
) -> Vec<ShellEntry<'a>> {
    let states = states_once(home, shells, unix_now());
    shells
        .iter()
        .map(|shell| {
            let last_activity_unix = activity.get(&shell.id).copied();
            match states.get(&shell.id) {
                Some(state) => ShellEntry {
                    shell,
                    mode: None,
                    activity: Some(state.activity.as_str()),
                    activity_since_unix: state
                        .since_unix
                        .filter(|_| state.activity != AgentActivity::Exited),
                    subagents_working: (state.subagents.working > 0)
                        .then_some(state.subagents.working),
                    subagent_kinds: state.subagents.kinds.clone(),
                    last_activity_unix,
                },
                None => ShellEntry {
                    shell,
                    mode: None,
                    activity: None,
                    activity_since_unix: None,
                    subagents_working: None,
                    subagent_kinds: Vec::new(),
                    last_activity_unix,
                },
            }
        })
        .collect()
}

/// `shell_entries` for terminal orchestrators, each marked `"mode":"terminal"`.
pub fn orchestrator_entries<'a>(
    home: &Path,
    shells: &'a [ShellSession],
    activity: &SessionActivity,
) -> Vec<ShellEntry<'a>> {
    let mut entries = shell_entries(home, shells, activity);
    for entry in &mut entries {
        entry.mode = Some("terminal");
    }
    entries
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
/// saw one of its files change, when one of its shells last had output, and
/// how many agents it has running.
#[derive(Serialize)]
pub struct ProjectEntry<'a> {
    #[serde(flatten)]
    project: &'a Project,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_edited_unix: Option<u64>,
    /// The newest `last_activity_unix` among the project's shells. Absent when
    /// none of them is live, or when tmux could not be asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_activity_unix: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agents: Option<AgentCounts>,
}

/// `agents` is left out for every project when the shell list could not be
/// read (no tmux), so "none" is never reported for "could not tell"; with no
/// shell list there is no `last_activity_unix` either.
pub fn project_entries<'a>(
    home: &Path,
    projects: &'a [Project],
    shells: Option<&[ShellSession]>,
    chats: &[ProjectFact],
    activity: &SessionActivity,
) -> Vec<ProjectEntry<'a>> {
    let recency = crate::recency_file::read(home);
    let counts = shells.map(|shells| {
        let mut counts = agent_counts(&states_once(home, shells, unix_now()), shells);
        count_chats(&mut counts, chats);
        counts
    });
    let active = shells.map(|shells| {
        let mut active = project_activity(shells, activity);
        chat_activity_times(&mut active, chats);
        active
    });
    projects
        .iter()
        .map(|project| ProjectEntry {
            project,
            last_edited_unix: recency
                .as_ref()
                .and_then(|published| published.projects.get(&project.id).copied()),
            last_activity_unix: active
                .as_ref()
                .and_then(|active| active.get(&project.id).copied()),
            agents: counts
                .as_ref()
                .map(|counts| counts.get(&project.id).copied().unwrap_or_default()),
        })
        .collect()
}

/// A project's chat orchestrator is one of its agents, as its terminal orchestrator is.
fn count_chats(counts: &mut BTreeMap<String, AgentCounts>, chats: &[ProjectFact]) {
    for chat in chats {
        let entry = counts.entry(chat.project_id.clone()).or_default();
        match chat.activity {
            AgentActivity::Working => entry.working += 1,
            AgentActivity::Waiting => entry.waiting += 1,
            AgentActivity::Done => entry.done += 1,
            AgentActivity::Unknown | AgentActivity::Exited => {}
        }
    }
}

/// And its chat's log growing is activity of the project.
fn chat_activity_times(newest: &mut BTreeMap<String, u64>, chats: &[ProjectFact]) {
    for chat in chats {
        if let Some(time) = chat.last_activity_unix {
            let entry = newest.entry(chat.project_id.clone()).or_default();
            *entry = (*entry).max(time);
        }
    }
}

/// The newest activity per project id over every shell the project owns, its
/// orchestrator included. The global orchestrator belongs to no project.
fn project_activity(shells: &[ShellSession], activity: &SessionActivity) -> BTreeMap<String, u64> {
    let mut newest: BTreeMap<String, u64> = BTreeMap::new();
    for shell in shells {
        let (Some(project), Some(&time)) = (shell.project_id.as_ref(), activity.get(&shell.id))
        else {
            continue;
        };
        let entry = newest.entry(project.clone()).or_default();
        *entry = (*entry).max(time);
    }
    newest
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

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(id: &str, project: Option<&str>, kind: &str) -> ShellSession {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "project_id": project,
            "worktree_id": null,
            "kind": kind,
            "cwd": "/tmp",
            "command": null,
            "created_at_unix": 0
        }))
        .unwrap()
    }

    #[test]
    fn a_project_is_as_active_as_its_newest_live_shell_and_the_global_orchestrator_is_nobodys() {
        let shells = [
            shell("a1", Some("alpha"), "project"),
            shell("a2", Some("alpha"), "project"),
            shell("a-orchestrator", Some("alpha"), "orchestrator"),
            shell("b-gone", Some("beta"), "project"),
            shell("b-orchestrator", Some("beta"), "orchestrator"),
            shell("global", None, "orchestrator"),
            shell("c-gone", Some("gamma"), "project"),
        ];
        // Shells tmux does not have are not in the answer.
        let activity = SessionActivity::from([
            ("a1".into(), 100),
            ("a2".into(), 300),
            ("a-orchestrator".into(), 200),
            ("b-orchestrator".into(), 50),
            ("global".into(), 9_999),
        ]);
        assert_eq!(
            project_activity(&shells, &activity),
            BTreeMap::from([("alpha".into(), 300), ("beta".into(), 50)])
        );
        assert!(project_activity(&shells, &SessionActivity::new()).is_empty());
        assert!(project_activity(&[], &activity).is_empty());
    }

    #[test]
    fn a_chat_orchestrator_counts_for_its_project_as_a_terminal_one_does() {
        let fact = |project: &str, activity, time| ProjectFact {
            project_id: project.into(),
            activity,
            last_activity_unix: time,
        };
        let mut counts = BTreeMap::new();
        count_chats(
            &mut counts,
            &[
                fact("alpha", AgentActivity::Working, Some(500)),
                fact("beta", AgentActivity::Done, None),
                fact("gamma", AgentActivity::Waiting, Some(10)),
                fact("delta", AgentActivity::Exited, Some(20)),
                fact("epsilon", AgentActivity::Unknown, None),
            ],
        );
        assert_eq!(
            counts["alpha"],
            AgentCounts {
                working: 1,
                waiting: 0,
                done: 0
            }
        );
        assert_eq!(
            counts["beta"],
            AgentCounts {
                working: 0,
                waiting: 0,
                done: 1
            }
        );
        assert_eq!(
            counts["gamma"],
            AgentCounts {
                working: 0,
                waiting: 1,
                done: 0
            }
        );
        // A chat whose agent is gone, or not started, counts as no agent at work.
        assert_eq!(counts["delta"], AgentCounts::default());
        assert_eq!(counts["epsilon"], AgentCounts::default());

        // Its log growing is the project's activity, beside the project's shells.
        let mut newest = BTreeMap::from([("alpha".to_owned(), 300), ("gamma".to_owned(), 50)]);
        chat_activity_times(
            &mut newest,
            &[
                fact("alpha", AgentActivity::Working, Some(500)),
                fact("gamma", AgentActivity::Waiting, Some(10)),
                fact("beta", AgentActivity::Done, None),
                fact("delta", AgentActivity::Exited, Some(20)),
            ],
        );
        assert_eq!(
            newest,
            BTreeMap::from([
                ("alpha".to_owned(), 500),
                ("delta".to_owned(), 20),
                ("gamma".to_owned(), 50)
            ])
        );
    }

    #[test]
    fn a_shell_entry_carries_its_own_time_only_while_it_has_one() {
        let shells = [
            shell("live", Some("alpha"), "project"),
            shell("gone", Some("alpha"), "project"),
        ];
        let activity = SessionActivity::from([("live".into(), 1_791_000_000)]);
        let entries = shell_entries(Path::new("/nonexistent"), &shells, &activity);
        let json = serde_json::to_value(&entries).unwrap();
        assert_eq!(json[0]["last_activity_unix"], 1_791_000_000u64);
        assert!(json[1].get("last_activity_unix").is_none(), "{}", json[1]);
        // A plain shell has no agent fields, but it has its time.
        assert!(json[0].get("activity").is_none());
    }
}
