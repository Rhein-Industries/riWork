//! Ordered, shared preferences for the full-width status bar.

use std::collections::HashSet;

use gpui::{AnyElement, Context, IntoElement, Window, div, prelude::*, rgb};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{
    theme::{self, Palette},
    tooltip::{self, Look},
    ui_text,
};

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
            Self::GlobalOrchestrator => "Open the global orchestrator.",
            Self::ProjectOrchestrator => "Open this project's orchestrator.",
            Self::Layout => "Apply the default layout or gather tabs into the main pane.",
        }
    }

    fn key(self) -> &'static str {
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
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
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

/// The status bar block of a settings page: the master switch with a reset
/// button, then the items of each side. The caller supplies the surrounding
/// card, heading and description. The owner persists each new snapshot and
/// shares it with all windows through the same settings path as the other
/// appearance preferences.
pub fn render_settings<V: 'static>(
    settings: &StatusBarSettings,
    on_change: impl Fn(&mut V, StatusBarSettings, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let settings = settings.normalized();
    let reset = on_change.clone();
    let master_change = on_change.clone();
    let master_settings = settings.clone();
    let master_enabled = settings.enabled;
    let mut card = div()
        .id("status-bar-settings")
        .flex()
        .flex_col()
        .gap(ui_text::space(9.0))
        .child(
            div()
                .flex()
                .gap(ui_text::space(8.0))
                .child(
                    div()
                        .id("status-bar-visible")
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(8.0))
                        .px(ui_text::space(8.0))
                        .py(ui_text::space(7.0))
                        .cursor_pointer()
                        .bg(rgb(colors.panel_active))
                        .text_size(ui_text::text(11.0))
                        .text_color(rgb(colors.text))
                        .hover(|style| style.bg(rgb(colors.divider)))
                        .child(check_box(master_enabled, colors))
                        .child("Show status bar")
                        .on_click(cx.listener(move |view, _, window, cx| {
                            let mut next = master_settings.clone();
                            next.enabled = !master_enabled;
                            master_change(view, next, window, cx);
                        })),
                )
                .child(
                    div()
                        .id("status-bar-reset")
                        .flex_none()
                        .flex()
                        .items_center()
                        .px(ui_text::space(8.0))
                        .cursor_pointer()
                        .border_1()
                        .border_color(rgb(colors.divider))
                        .text_size(ui_text::text(9.0))
                        .text_color(rgb(colors.cyan))
                        .hover(|style| {
                            style
                                .bg(rgb(colors.panel_active))
                                .border_color(rgb(colors.cyan))
                        })
                        .child("RESET DEFAULTS")
                        .on_click(cx.listener(move |view, _, window, cx| {
                            reset(view, StatusBarSettings::default(), window, cx)
                        })),
                ),
        );
    for side in [StatusSide::Left, StatusSide::Right] {
        let items: Vec<_> = settings
            .items
            .iter()
            .copied()
            .filter(|item| item.side == side)
            .collect();
        card = card.child(
            div()
                .flex()
                .flex_col()
                .gap(ui_text::space(4.0))
                .child(
                    div()
                        .pt(ui_text::space(4.0))
                        .text_size(ui_text::text(9.0))
                        .text_color(rgb(if side == StatusSide::Left {
                            colors.cyan
                        } else {
                            colors.magenta
                        }))
                        .child(side.label()),
                )
                .children(items.iter().enumerate().map(|(index, item)| {
                    render_item(
                        *item,
                        index > 0,
                        index + 1 < items.len(),
                        &settings,
                        on_change.clone(),
                        cx,
                    )
                })),
        );
    }
    card.into_any_element()
}

fn render_item<V: 'static>(
    item: StatusBarItem,
    can_move_up: bool,
    can_move_down: bool,
    settings: &StatusBarSettings,
    on_change: impl Fn(&mut V, StatusBarSettings, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let toggle = on_change.clone();
    let toggle_settings = settings.clone();
    let route = on_change.clone();
    let route_settings = settings.clone();
    div()
        .flex()
        .items_center()
        .gap(ui_text::space(7.0))
        .px(ui_text::space(7.0))
        .py(ui_text::space(5.0))
        .bg(rgb(colors.panel_active))
        .border_1()
        .border_color(rgb(colors.divider))
        .child(
            div()
                .id(format!("status-item-{}-visible", item.kind.key()))
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(ui_text::space(8.0))
                .cursor_pointer()
                .hover(|style| style.text_color(rgb(colors.cyan)))
                .child(check_box(item.enabled, colors))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(ui_text::text(11.0))
                        .text_color(rgb(if item.enabled {
                            colors.text
                        } else {
                            colors.muted
                        }))
                        .text_ellipsis()
                        .child(item.kind.label()),
                )
                .child(tooltip::anchor(item.kind.description(), Look::Status))
                .on_click(cx.listener(move |view, _, window, cx| {
                    let mut next = toggle_settings.clone();
                    next.set_visible(item.kind, !item.enabled);
                    toggle(view, next, window, cx);
                })),
        )
        .child(
            div()
                .id(format!("status-item-{}-side", item.kind.key()))
                .flex_none()
                .px(ui_text::space(5.0))
                .py(ui_text::space(4.0))
                .border_1()
                .border_color(rgb(colors.divider))
                .cursor_pointer()
                .text_size(ui_text::text(9.0))
                .text_color(rgb(if item.side == StatusSide::Left {
                    colors.cyan
                } else {
                    colors.magenta
                }))
                .hover(|style| style.border_color(rgb(colors.cyan)))
                .child(item.side.label())
                .child(tooltip::anchor(
                    if item.side == StatusSide::Left {
                        "Move to the right side"
                    } else {
                        "Move to the left side"
                    },
                    Look::Status,
                ))
                .on_click(cx.listener(move |view, _, window, cx| {
                    let mut next = route_settings.clone();
                    next.set_side(item.kind, item.side.opposite());
                    route(view, next, window, cx);
                })),
        )
        .child(move_button(
            item.kind,
            true,
            can_move_up,
            settings,
            on_change.clone(),
            cx,
        ))
        .child(move_button(
            item.kind,
            false,
            can_move_down,
            settings,
            on_change,
            cx,
        ))
        .into_any_element()
}

fn move_button<V: 'static>(
    kind: StatusItemKind,
    up: bool,
    enabled: bool,
    settings: &StatusBarSettings,
    on_change: impl Fn(&mut V, StatusBarSettings, &mut Window, &mut Context<V>) + Clone + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let colors = theme::palette(cx);
    let settings = settings.clone();
    div()
        .id(format!(
            "status-item-{}-{}",
            kind.key(),
            if up { "up" } else { "down" }
        ))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(ui_text::space(22.0))
        .h(ui_text::space(22.0))
        .text_size(ui_text::text(11.0))
        .text_color(rgb(if enabled { colors.cyan } else { colors.muted }))
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(rgb(colors.divider)))
        })
        .child(if up { "↑" } else { "↓" })
        .child(tooltip::anchor(
            if up {
                "Move earlier on this side"
            } else {
                "Move later on this side"
            },
            Look::Status,
        ))
        .on_click(cx.listener(move |view, _, window, cx| {
            if enabled {
                let mut next = settings.clone();
                if next.move_item(kind, up) {
                    on_change(view, next, window, cx);
                }
            }
        }))
        .into_any_element()
}

fn check_box(enabled: bool, colors: Palette) -> AnyElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(ui_text::space(12.0))
        .border_1()
        .border_color(rgb(if enabled { colors.cyan } else { colors.muted }))
        .text_size(ui_text::text(9.0))
        .text_color(rgb(colors.cyan))
        .child(if enabled { "✓" } else { "" })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_keeps_current_project_left_and_existing_readouts_right() {
        let settings = StatusBarSettings::default();
        assert_eq!(
            settings.visible_items(StatusSide::Left),
            [StatusItemKind::Project]
        );
        assert_eq!(
            settings.visible_items(StatusSide::Right),
            [
                StatusItemKind::LiveSessions,
                StatusItemKind::Resources,
                StatusItemKind::Usage,
                StatusItemKind::CodexAccount,
                StatusItemKind::SessionId,
                StatusItemKind::GlobalOrchestrator,
                StatusItemKind::ProjectOrchestrator,
                StatusItemKind::Layout
            ]
        );
        assert!(
            !settings
                .items
                .iter()
                .find(|item| item.kind == StatusItemKind::Worktree)
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn custom_order_deduplicates_without_enabling_other_missing_items() {
        let mut settings = StatusBarSettings {
            enabled: true,
            items: vec![
                StatusBarItem {
                    kind: StatusItemKind::Usage,
                    enabled: true,
                    side: StatusSide::Left,
                },
                StatusBarItem {
                    kind: StatusItemKind::Project,
                    enabled: false,
                    side: StatusSide::Right,
                },
                StatusBarItem {
                    kind: StatusItemKind::Usage,
                    enabled: false,
                    side: StatusSide::Right,
                },
            ],
        };
        settings.normalize();
        assert_eq!(settings.items.len(), StatusItemKind::ALL.len());
        assert_eq!(
            settings.visible_items(StatusSide::Left),
            [StatusItemKind::Usage]
        );
        assert_eq!(
            settings.visible_items(StatusSide::Right),
            [StatusItemKind::CodexAccount, StatusItemKind::Layout]
        );
        let before = settings.clone();
        settings.normalize();
        assert_eq!(settings, before);
    }

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
    fn the_layout_menu_is_on_by_default_and_ends_the_right_side() {
        let settings = StatusBarSettings::default();
        let layout = settings
            .items
            .iter()
            .find(|item| item.kind == StatusItemKind::Layout)
            .unwrap();
        assert!(layout.enabled);
        assert_eq!(layout.side, StatusSide::Right);
        assert_eq!(settings.items.last().unwrap().kind, StatusItemKind::Layout);
        assert_eq!(
            settings.visible_items(StatusSide::Right).last(),
            Some(&StatusItemKind::Layout)
        );
        // It has a name and a description for Settings, and its own saved name.
        assert_eq!(StatusItemKind::Layout.label(), "Layout menu");
        assert!(!StatusItemKind::Layout.description().is_empty());
        assert!(
            serde_json::to_string(&settings)
                .unwrap()
                .contains(r#""kind":"layout""#)
        );
    }

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

    #[test]
    fn hiding_moving_or_routing_the_layout_menu_is_kept_across_a_save_and_a_load() {
        let mut settings: StatusBarSettings = serde_json::from_str(BEFORE_THE_LAYOUT_MENU).unwrap();
        settings.set_visible(StatusItemKind::Layout, false);
        let raw = serde_json::to_string(&settings).unwrap();
        let hidden: StatusBarSettings = serde_json::from_str(&raw).unwrap();
        assert!(
            !hidden
                .visible_items(StatusSide::Right)
                .contains(&StatusItemKind::Layout)
        );
        assert_eq!(hidden, settings);

        // Shown again, moved two places earlier (the first step passes the project
        // orchestrator, which the old bar hid), then to the left.
        let mut settings = hidden;
        settings.set_visible(StatusItemKind::Layout, true);
        assert!(settings.move_item(StatusItemKind::Layout, true));
        assert!(settings.move_item(StatusItemKind::Layout, true));
        let moved: StatusBarSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(
            moved.visible_items(StatusSide::Right),
            [
                StatusItemKind::LiveSessions,
                StatusItemKind::Usage,
                StatusItemKind::CodexAccount,
                StatusItemKind::SessionId,
                StatusItemKind::Layout,
                StatusItemKind::GlobalOrchestrator
            ]
        );
        let mut left = moved;
        left.set_side(StatusItemKind::Layout, StatusSide::Left);
        assert_eq!(
            left.visible_items(StatusSide::Left).last(),
            Some(&StatusItemKind::Layout)
        );
        assert!(
            !left
                .visible_items(StatusSide::Right)
                .contains(&StatusItemKind::Layout)
        );

        // With the whole bar off, nothing shows.
        left.enabled = false;
        assert!(left.visible_items(StatusSide::Left).is_empty());
    }

    #[test]
    fn a_bar_emptied_on_purpose_does_not_get_the_layout_menu_back() {
        let empty: StatusBarSettings = serde_json::from_str(r#"{"items":[]}"#).unwrap();
        assert!(empty.visible_items(StatusSide::Right).is_empty());
        assert!(
            empty
                .items
                .iter()
                .find(|item| item.kind == StatusItemKind::Layout)
                .is_some_and(|item| !item.enabled)
        );
    }
}
