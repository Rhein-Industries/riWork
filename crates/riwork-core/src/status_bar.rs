//! Ordered, shared preferences for the full-width status bar.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusItemKind {
    Project,
    Worktree,
    AgentActivity,
    LiveSessions,
    Resources,
    Usage,
    CodexAccount,
    SessionId,
    GlobalOrchestrator,
    ProjectOrchestrator,
    /// The layout menu: apply the default layout, gather tabs into the main pane.
    Layout,
}

impl StatusItemKind {
    pub const ALL: [Self; 11] = [
        Self::Project,
        Self::Worktree,
        Self::AgentActivity,
        Self::LiveSessions,
        Self::Resources,
        Self::Usage,
        Self::CodexAccount,
        Self::SessionId,
        Self::GlobalOrchestrator,
        Self::ProjectOrchestrator,
        Self::Layout,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "Current project",
            Self::Worktree => "Current worktree",
            Self::AgentActivity => "Agent activity",
            Self::LiveSessions => "Live sessions",
            Self::Resources => "CPU and memory",
            Self::Usage => "Account usage",
            Self::CodexAccount => "Codex account email",
            Self::SessionId => "Session ID",
            Self::GlobalOrchestrator => "Global orchestrator",
            Self::ProjectOrchestrator => "Project orchestrator",
            Self::Layout => "Layout menu",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Project => "The project selected in this window.",
            Self::Worktree => "The current worktree's branch.",
            Self::AgentActivity => "Whether agents are working, waiting, or done.",
            Self::LiveSessions => "Running sessions in the current project.",
            Self::Resources => "CPU and memory used by the project's sessions.",
            Self::Usage => "The active account's remaining quota.",
            Self::CodexAccount => "Focused Codex account email, or the configured project default.",
            Self::SessionId => "The active session's ID; click to copy it.",
            Self::GlobalOrchestrator => "Open the global dashboard or the global chat.",
            Self::ProjectOrchestrator => "Open this project's orchestrator.",
            Self::Layout => "Apply the default layout or gather tabs into the main pane.",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Worktree => "worktree",
            Self::AgentActivity => "agent-activity",
            Self::LiveSessions => "live-sessions",
            Self::Resources => "resources",
            Self::Usage => "usage",
            Self::CodexAccount => "codex-account",
            Self::SessionId => "session-id",
            Self::GlobalOrchestrator => "global-orchestrator",
            Self::ProjectOrchestrator => "project-orchestrator",
            Self::Layout => "layout",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusSide {
    Left,
    Right,
}

impl StatusSide {
    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
        }
    }

    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct StatusBarItem {
    pub kind: StatusItemKind,
    pub enabled: bool,
    pub side: StatusSide,
}

impl StatusBarItem {
    fn default_for(kind: StatusItemKind) -> Self {
        Self {
            kind,
            enabled: !matches!(
                kind,
                StatusItemKind::Worktree | StatusItemKind::AgentActivity
            ),
            side: if matches!(
                kind,
                StatusItemKind::Project | StatusItemKind::Worktree | StatusItemKind::AgentActivity
            ) {
                StatusSide::Left
            } else {
                StatusSide::Right
            },
        }
    }
}

impl<'de> Deserialize<'de> for StatusBarItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            kind: StatusItemKind,
            enabled: Option<bool>,
            side: Option<StatusSide>,
        }
        let fields = Fields::deserialize(deserializer)?;
        let defaults = Self::default_for(fields.kind);
        Ok(Self {
            kind: fields.kind,
            enabled: fields.enabled.unwrap_or(defaults.enabled),
            side: fields.side.unwrap_or(defaults.side),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StatusBarSettings {
    pub enabled: bool,
    pub items: Vec<StatusBarItem>,
}

impl Default for StatusBarSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            items: StatusItemKind::ALL
                .into_iter()
                .map(StatusBarItem::default_for)
                .collect(),
        }
    }
}

impl<'de> Deserialize<'de> for StatusBarSettings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(object) = value.as_object() else {
            return Ok(Self::default());
        };
        let mut settings = Self::default();
        settings.enabled = object
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if let Some(items) = object.get("items").and_then(Value::as_array) {
            // Preserve known choices if a newer version wrote additional kinds,
            // or one entry is damaged; one bad row must not reset preferences.
            settings.items = items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect();
        }
        settings.normalize();
        Ok(settings)
    }
}

impl StatusBarSettings {
    /// First occurrence wins. New kinds stay disabled in customized bars,
    /// except the account readout and the layout menu, which were requested for
    /// existing installations. Both are appended last, so the layout menu ends
    /// the right side; hiding or moving either is kept like any other choice.
    pub fn normalize(&mut self) {
        let had_items = !self.items.is_empty();
        let mut seen = HashSet::new();
        self.items.retain(|item| seen.insert(item.kind));
        for kind in StatusItemKind::ALL {
            if seen.insert(kind) {
                self.items.push(StatusBarItem {
                    // Introduce the requested readouts to older saved bars
                    // while retaining their other customized choices.
                    enabled: had_items
                        && matches!(kind, StatusItemKind::CodexAccount | StatusItemKind::Layout),
                    ..StatusBarItem::default_for(kind)
                });
            }
        }
    }

    pub fn normalized(&self) -> Self {
        let mut settings = self.clone();
        settings.normalize();
        settings
    }

    pub fn visible_items(&self, side: StatusSide) -> Vec<StatusItemKind> {
        if !self.enabled {
            return Vec::new();
        }
        self.normalized()
            .items
            .into_iter()
            .filter(|item| item.enabled && item.side == side)
            .map(|item| item.kind)
            .collect()
    }

    pub fn set_visible(&mut self, kind: StatusItemKind, enabled: bool) {
        self.normalize();
        if let Some(item) = self.items.iter_mut().find(|item| item.kind == kind) {
            item.enabled = enabled;
        }
    }

    pub fn set_side(&mut self, kind: StatusItemKind, side: StatusSide) {
        self.normalize();
        if let Some(item) = self.items.iter_mut().find(|item| item.kind == kind) {
            item.side = side;
        }
    }

    /// Reorder within the chosen side, preserving every other side's order.
    pub fn move_item(&mut self, kind: StatusItemKind, up: bool) -> bool {
        self.normalize();
        let Some(index) = self.items.iter().position(|item| item.kind == kind) else {
            return false;
        };
        let side = self.items[index].side;
        let neighbor = if up {
            (0..index)
                .rev()
                .find(|&candidate| self.items[candidate].side == side)
        } else {
            ((index + 1)..self.items.len()).find(|&candidate| self.items[candidate].side == side)
        };
        if let Some(neighbor) = neighbor {
            self.items.swap(index, neighbor);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_preferences_and_unknown_items_preserve_known_choices() {
        let settings: StatusBarSettings = serde_json::from_str(r#"{"items":[{"kind":"project"},{"kind":"usage","side":"left","enabled":false},{"kind":"future_widget","enabled":true},{"kind":"resources","enabled":"broken"}]}"#).unwrap();
        assert_eq!(
            settings.visible_items(StatusSide::Left),
            [StatusItemKind::Project]
        );
        assert_eq!(
            settings.visible_items(StatusSide::Right),
            [StatusItemKind::CodexAccount, StatusItemKind::Layout]
        );
        assert_eq!(settings.items.len(), StatusItemKind::ALL.len());
        assert_eq!(
            serde_json::from_str::<StatusBarSettings>("{}").unwrap(),
            StatusBarSettings::default()
        );
        assert_eq!(
            serde_json::from_str::<StatusBarSettings>("null").unwrap(),
            StatusBarSettings::default()
        );
        let empty: StatusBarSettings = serde_json::from_str(r#"{"items":[]}"#).unwrap();
        assert!(empty.visible_items(StatusSide::Left).is_empty());
        assert!(empty.visible_items(StatusSide::Right).is_empty());
    }

    #[test]
    fn routing_and_reordering_preserve_other_side_and_round_trip() {
        let mut settings = StatusBarSettings::default();
        settings.set_visible(StatusItemKind::Worktree, true);
        settings.set_side(StatusItemKind::Usage, StatusSide::Left);
        let right_before = settings.visible_items(StatusSide::Right);
        assert!(settings.move_item(StatusItemKind::Usage, true));
        assert_eq!(
            settings.visible_items(StatusSide::Left),
            [
                StatusItemKind::Project,
                StatusItemKind::Worktree,
                StatusItemKind::Usage
            ]
        );
        // The disabled activity item has a saved position too. Moving past it
        // again changes the visible order without losing its preference.
        assert!(settings.move_item(StatusItemKind::Usage, true));
        assert_eq!(
            settings.visible_items(StatusSide::Left),
            [
                StatusItemKind::Project,
                StatusItemKind::Usage,
                StatusItemKind::Worktree
            ]
        );
        assert_eq!(settings.visible_items(StatusSide::Right), right_before);
        assert!(!settings.move_item(StatusItemKind::Project, true));
        let raw = serde_json::to_string(&settings).unwrap();
        assert_eq!(
            serde_json::from_str::<StatusBarSettings>(&raw).unwrap(),
            settings
        );
        settings.enabled = false;
        assert!(settings.visible_items(StatusSide::Left).is_empty());
        assert!(settings.visible_items(StatusSide::Right).is_empty());
        settings.enabled = true;
        assert_eq!(settings.visible_items(StatusSide::Right), right_before);
    }

    /// A bar as a build from before the layout menu saved it: ten items, in the old default
    /// order, with the choices below.
    const BEFORE_THE_LAYOUT_MENU: &str = r#"{"enabled":true,"items":[
        {"kind":"project","enabled":true,"side":"left"},
        {"kind":"worktree","enabled":true,"side":"left"},
        {"kind":"agent_activity","enabled":false,"side":"left"},
        {"kind":"live_sessions","enabled":true,"side":"right"},
        {"kind":"resources","enabled":false,"side":"right"},
        {"kind":"usage","enabled":true,"side":"right"},
        {"kind":"codex_account","enabled":true,"side":"right"},
        {"kind":"session_id","enabled":true,"side":"right"},
        {"kind":"global_orchestrator","enabled":true,"side":"right"},
        {"kind":"project_orchestrator","enabled":false,"side":"right"}]}"#;

    #[test]
    fn a_bar_saved_before_the_layout_menu_gets_it_last_on_the_right_and_keeps_every_choice() {
        let settings: StatusBarSettings = serde_json::from_str(BEFORE_THE_LAYOUT_MENU).unwrap();
        assert_eq!(settings.items.len(), StatusItemKind::ALL.len());
        // The menu was requested for existing installations, so it is shown, after the
        // orchestrators, on the right.
        assert_eq!(
            settings.visible_items(StatusSide::Right),
            [
                StatusItemKind::LiveSessions,
                StatusItemKind::Usage,
                StatusItemKind::CodexAccount,
                StatusItemKind::SessionId,
                StatusItemKind::GlobalOrchestrator,
                StatusItemKind::Layout
            ]
        );
        // What the old bar hid, showed and routed stays as it was.
        assert_eq!(
            settings.visible_items(StatusSide::Left),
            [StatusItemKind::Project, StatusItemKind::Worktree]
        );

        // The first save writes it as a choice of its own, and it reads back the same.
        let raw = serde_json::to_string(&settings).unwrap();
        let reread: StatusBarSettings = serde_json::from_str(&raw).unwrap();
        assert_eq!(reread, settings);
        assert_eq!(settings.items.last().unwrap().kind, StatusItemKind::Layout);
    }
}
