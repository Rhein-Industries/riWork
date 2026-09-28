//! Deterministic project ordering, independent of folder structure and filtering.

use std::{cmp::Ordering, collections::BTreeMap};

use serde::{Deserialize, Serialize};

use crate::{sessions::ShellSession, store::State};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSort {
    Name,
    LastEdited,
    DateAdded,
    LiveSessions,
}

impl ProjectSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::LastEdited => "Last edited",
            Self::DateAdded => "Date added",
            Self::LiveSessions => "Live sessions",
        }
    }

    pub fn default_descending(self) -> bool {
        !matches!(self, Self::Name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectOrder {
    pub by: ProjectSort,
    pub descending: bool,
}

impl Default for ProjectOrder {
    fn default() -> Self {
        Self::for_sort(ProjectSort::LastEdited)
    }
}

impl ProjectOrder {
    pub fn for_sort(by: ProjectSort) -> Self {
        Self {
            by,
            descending: by.default_descending(),
        }
    }

    pub fn toggled(self) -> Self {
        Self {
            descending: !self.descending,
            ..self
        }
    }

    pub fn direction_label(self) -> &'static str {
        match (self.by, self.descending) {
            (ProjectSort::Name, false) => "A to Z",
            (ProjectSort::Name, true) => "Z to A",
            (ProjectSort::LastEdited | ProjectSort::DateAdded, true) => "Newest first",
            (ProjectSort::LastEdited | ProjectSort::DateAdded, false) => "Oldest first",
            (ProjectSort::LiveSessions, true) => "Most live sessions first",
            (ProjectSort::LiveSessions, false) => "Fewest live sessions first",
        }
    }
}

/// Return all project indices in order. Grouping consumes this order without moving
/// projects between folders; unknown timestamps always follow known timestamps.
pub fn sorted_project_indices(
    state: &State,
    order: ProjectOrder,
    last_edits: &BTreeMap<String, u64>,
    shells: &[ShellSession],
) -> Vec<usize> {
    let mut live = BTreeMap::<&str, u64>::new();
    if order.by == ProjectSort::LiveSessions {
        for shell in shells.iter().filter(|shell| shell.alive) {
            if let Some(project_id) = shell.project_id.as_deref() {
                *live.entry(project_id).or_default() += 1;
            }
        }
    }
    let names: Vec<_> = state
        .projects
        .iter()
        .map(|project| project.name.to_lowercase())
        .collect();
    let values: Vec<_> = state
        .projects
        .iter()
        .map(|project| match order.by {
            ProjectSort::Name => None,
            ProjectSort::LastEdited => last_edits
                .get(&project.id)
                .copied()
                .filter(|time| *time > 0),
            ProjectSort::DateAdded => (project.created_at > 0).then_some(project.created_at),
            ProjectSort::LiveSessions => Some(live.get(project.id.as_str()).copied().unwrap_or(0)),
        })
        .collect();
    let mut indices: Vec<_> = (0..state.projects.len()).collect();
    indices.sort_by(|&a, &b| {
        let left = &state.projects[a];
        let right = &state.projects[b];
        let name_order = || {
            names[a]
                .cmp(&names[b])
                .then_with(|| left.name.cmp(&right.name))
        };
        let primary = if order.by == ProjectSort::Name {
            directed(name_order(), order.descending)
        } else {
            match (values[a], values[b]) {
                (Some(a), Some(b)) => directed(a.cmp(&b), order.descending),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        };
        primary
            .then_with(name_order)
            .then_with(|| left.id.cmp(&right.id))
    });
    indices
}

fn directed(ordering: Ordering, descending: bool) -> Ordering {
    if descending {
        ordering.reverse()
    } else {
        ordering
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{sessions::ShellKind, store::Project};
    use std::path::PathBuf;

    fn project(id: &str, name: &str, added: u64) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            root: PathBuf::from(format!("/projects/{id}")),
            repository_roots: Vec::new(),
            folder_id: None,
            notify_on_agent_done: false,
            codex_account: crate::store::ProjectCodexAccount::default(),
            created_at: added,
        }
    }

    fn ids(
        state: &State,
        order: ProjectOrder,
        edits: &BTreeMap<String, u64>,
        shells: &[ShellSession],
    ) -> Vec<String> {
        sorted_project_indices(state, order, edits, shells)
            .into_iter()
            .map(|index| state.projects[index].id.clone())
            .collect()
    }

    #[test]
    fn timestamps_sort_both_ways_with_unknown_last_and_stable_ties() {
        let state = State {
            projects: vec![
                project("missing", "A missing", 0),
                project("z", "Zulu", 10),
                project("a2", "Alpha", 20),
                project("a1", "Alpha", 20),
                project("zero", "B zero", 0),
            ],
            ..State::default()
        };
        let edits = BTreeMap::from([
            ("z".into(), 10),
            ("a2".into(), 20),
            ("a1".into(), 20),
            ("zero".into(), 0),
        ]);
        for by in [ProjectSort::LastEdited, ProjectSort::DateAdded] {
            let order = ProjectOrder::for_sort(by);
            assert_eq!(
                ids(&state, order, &edits, &[]),
                ["a1", "a2", "z", "missing", "zero"]
            );
            assert_eq!(
                ids(&state, order.toggled(), &edits, &[]),
                ["z", "a1", "a2", "missing", "zero"]
            );
        }
        assert_eq!(
            ids(&state, ProjectOrder::default(), &BTreeMap::new(), &[]),
            ["missing", "a1", "a2", "zero", "z"]
        );
    }

    #[test]
    fn name_sort_is_case_insensitive_with_name_and_id_tie_breaks() {
        let state = State {
            projects: vec![
                project("b", "beta", 0),
                project("a2", "Alpha", 0),
                project("a1", "Alpha", 0),
                project("a3", "alpha", 0),
            ],
            ..State::default()
        };
        let order = ProjectOrder::for_sort(ProjectSort::Name);
        assert_eq!(
            ids(&state, order, &BTreeMap::new(), &[]),
            ["a1", "a2", "a3", "b"]
        );
        assert_eq!(
            ids(&state, order.toggled(), &BTreeMap::new(), &[]),
            ["b", "a3", "a1", "a2"]
        );
    }

    #[test]
    fn live_sessions_count_only_alive_project_scoped_shells() {
        let state = State {
            projects: vec![
                project("a", "Alpha", 0),
                project("b", "Beta", 0),
                project("c", "Charlie", 0),
            ],
            ..State::default()
        };
        let shell = |id: &str, project_id: Option<&str>, alive, kind| ShellSession {
            id: id.into(),
            project_id: project_id.map(str::to_owned),
            worktree_id: None,
            kind,
            cwd: PathBuf::from("/"),
            command: None,
            editor_path: None,
            harness: None,
            codex_account_id: None,
            codex_account_label: None,
            codex_account_email: None,
            codex_home: None,
            unrestricted: false,
            orchestrator_skill_loaded: false,
            orchestrator_skill_version: None,
            orchestrator_project_root: None,
            created_at_unix: 0,
            alive,
        };
        let shells = vec![
            shell("b-shell", Some("b"), true, ShellKind::Project),
            shell("b-orch", Some("b"), true, ShellKind::Orchestrator),
            shell("a-shell", Some("a"), true, ShellKind::Project),
            shell("dead", Some("a"), false, ShellKind::Project),
            shell("global", None, true, ShellKind::Orchestrator),
            shell("foreign", Some("unknown"), true, ShellKind::Project),
        ];
        let order = ProjectOrder::for_sort(ProjectSort::LiveSessions);
        assert_eq!(
            ids(&state, order, &BTreeMap::new(), &shells),
            ["b", "a", "c"]
        );
        assert_eq!(
            ids(&state, order.toggled(), &BTreeMap::new(), &shells),
            ["c", "a", "b"]
        );
    }

    #[test]
    fn preferences_round_trip_and_defaults_choose_recent_files() {
        assert_eq!(
            ProjectOrder::default(),
            ProjectOrder {
                by: ProjectSort::LastEdited,
                descending: true
            }
        );
        assert_eq!(
            serde_json::from_str::<ProjectOrder>("{}").unwrap(),
            ProjectOrder::default()
        );
        let order = ProjectOrder::for_sort(ProjectSort::Name).toggled();
        assert_eq!(
            serde_json::from_str::<ProjectOrder>(&serde_json::to_string(&order).unwrap()).unwrap(),
            order
        );
    }
}
