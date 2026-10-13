//! Serialized project-order preferences; runtime sorting lives in the engine.

use serde::{Deserialize, Serialize};

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
            Self::Name => "Project name",
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
