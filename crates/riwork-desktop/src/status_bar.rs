//! Ordered, shared preferences for the full-width status bar.

use gpui::{AnyElement, Context, FontWeight, IntoElement, Window, div, prelude::*, rgb};

use crate::{
    behavior_controls as behavior, controls, icons,
    theme::{self, Palette},
    tooltip::{self, Look},
    ui_text,
};

pub use riwork_core::status_bar::{StatusBarItem, StatusBarSettings, StatusItemKind, StatusSide};

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
                    behavior::switch_content(
                        "status-bar-visible",
                        "Show status bar",
                        if ui_text::is_native() {
                            div().child("Show status bar").into_any_element()
                        } else {
                            check_box(master_enabled, colors)
                        },
                        master_enabled,
                    )
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(8.0))
                    .px(ui_text::space(8.0))
                    .py(ui_text::space(7.0))
                    .bg(rgb(colors.panel_active))
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.text))
                    .hover(move |style| {
                        controls::hovered(style, controls::row_hover(false, colors), |style| {
                            style.bg(rgb(colors.divider))
                        })
                    })
                    .map(|row| {
                        // Native: a row with a switch at its end, like the toggles above.
                        controls::native(row, |row| {
                            controls::row(row, false, colors).justify_between()
                        })
                    })
                    .when(!ui_text::is_native(), |row| row.child("Show status bar"))
                    .when(ui_text::is_native(), |row| {
                        row.child(controls::switch(master_enabled, colors))
                    })
                    .on_change({
                        let listener = cx.listener(move |view, checked: &bool, window, cx| {
                            let mut next = master_settings.clone();
                            next.enabled = *checked;
                            master_change(view, next, window, cx);
                        });
                        move |checked, _, window, cx| listener(&checked, window, cx)
                    })
                    .map(|control| {
                        crate::form_input::control_element("status-bar-visible", control)
                    }),
                )
                .child(
                    behavior::button_content(
                        "status-bar-reset",
                        "Reset defaults",
                        ui_text::cased("Reset defaults"),
                    )
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(ui_text::space(8.0))
                    .border_1()
                    .border_color(rgb(colors.divider))
                    .text_size(ui_text::text(9.0))
                    .text_color(rgb(colors.cyan))
                    .hover(move |style| {
                        controls::hovered(
                            style,
                            controls::Button::Secondary.hover(colors),
                            |style| {
                                style
                                    .bg(rgb(colors.panel_active))
                                    .border_color(rgb(colors.cyan))
                            },
                        )
                    })
                    .map(|button| {
                        controls::native(button, |button| {
                            controls::button(button, controls::Button::Secondary, colors)
                                .text_size(ui_text::text(10.0))
                        })
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        reset(view, StatusBarSettings::default(), window, cx)
                    }))
                    .map(|control| crate::form_input::control_element("status-bar-reset", control)),
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
                        .map(|heading| {
                            controls::native(heading, |heading| {
                                heading
                                    .px(ui_text::space(8.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(colors.muted))
                            })
                        })
                        .child(ui_text::cased(side.label())),
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
        .map(|row| controls::native(row, |row| controls::row(row, false, colors)))
        .child(
            behavior::switch_content(
                format!("status-item-{}-visible", item.kind.key()),
                item.kind.label(),
                check_box(item.enabled, colors),
                item.enabled,
            )
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(ui_text::space(8.0))
            .hover(|style| style.text_color(rgb(colors.cyan)))
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
            .on_change({
                let listener = cx.listener(move |view, checked: &bool, window, cx| {
                    let mut next = toggle_settings.clone();
                    next.set_visible(item.kind, *checked);
                    toggle(view, next, window, cx);
                });
                move |checked, _, window, cx| listener(&checked, window, cx)
            }),
        )
        .child(
            behavior::button_content(
                format!("status-item-{}-side", item.kind.key()),
                format!(
                    "Move {} to the {} side",
                    item.kind.label(),
                    item.side.opposite().label()
                ),
                ui_text::cased(item.side.label()),
            )
            .flex_none()
            .px(ui_text::space(5.0))
            .py(ui_text::space(4.0))
            .border_1()
            .border_color(rgb(colors.divider))
            .text_size(ui_text::text(9.0))
            .text_color(rgb(if item.side == StatusSide::Left {
                colors.cyan
            } else {
                colors.magenta
            }))
            .hover(move |style| {
                controls::hovered(style, controls::Button::Secondary.hover(colors), |style| {
                    style.border_color(rgb(colors.cyan))
                })
            })
            .map(|button| {
                controls::native(button, |button| {
                    controls::button(button, controls::Button::Secondary, colors)
                        .px(ui_text::space(9.0))
                        .py(ui_text::space(2.0))
                })
            })
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
            }))
            .map(|control| {
                crate::form_input::control_element(
                    format!("status-item-{}-side", item.kind.key()),
                    control,
                )
            }),
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
    behavior::button_content(
        format!(
            "status-item-{}-{}",
            kind.key(),
            if up { "up" } else { "down" }
        ),
        format!(
            "Move {} {}",
            kind.label(),
            if up { "earlier" } else { "later" }
        ),
        icons::mark(
            if up { "↑" } else { "↓" },
            10.0,
            if enabled { colors.cyan } else { colors.muted },
        ),
    )
    .disabled(!enabled)
    .flex_none()
    .flex()
    .items_center()
    .justify_center()
    .w(ui_text::space(22.0))
    .h(ui_text::space(22.0))
    .text_size(ui_text::text(11.0))
    .text_color(rgb(if enabled { colors.cyan } else { colors.muted }))
    .when(enabled, |button| {
        button.hover(|style| style.bg(rgb(colors.divider)))
    })
    .map(|button| controls::native(button, |button| button.rounded_full()))
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
    .map(|control| {
        crate::form_input::control_element(
            format!(
                "status-item-{}-{}",
                kind.key(),
                if up { "up" } else { "down" }
            ),
            control,
        )
    })
    .into_any_element()
}

fn check_box(enabled: bool, colors: Palette) -> AnyElement {
    if ui_text::is_native() {
        // A rounded box, filled with the primary color and ticked when on.
        return div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(ui_text::space(14.0))
            .rounded(ui_text::space(4.0))
            .border_1()
            .border_color(rgb(if enabled { colors.cyan } else { colors.divider }))
            .when(enabled, |check| check.bg(rgb(colors.cyan)))
            .when(enabled, |check| {
                check.child(icons::mark("✓", 8.0, colors.bg))
            })
            .into_any_element();
    }
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

#[cfg(test)]
mod kit_control_tests {
    use super::*;
    use crate::form_input::{test_turn, test_window};
    use gpui::{Render, TestAppContext};
    use gpui_kit::test::TestWindowExt;

    struct Fixture {
        settings: StatusBarSettings,
        changes: Vec<StatusBarSettings>,
    }
    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            render_settings(
                &self.settings,
                |owner: &mut Self, next, _, cx| {
                    owner.changes.push(next.clone());
                    owner.settings = next;
                    cx.notify();
                },
                cx,
            )
        }
    }
    fn mount(cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, gpui::Entity<Fixture>) {
        test_window(cx, |_, _| Fixture {
            settings: StatusBarSettings::default(),
            changes: Vec::new(),
        })
    }

    #[gpui::test]
    fn status_disabled_reorder_and_keyboard_reset_keep_domain_defaults(cx: &mut TestAppContext) {
        let (window, owner) = mount(cx);
        test_turn(cx, window, |window, app| {
            let disabled = crate::form_input::test_ax_node(window, app, "status-item-project-up");
            assert_eq!(disabled.role(), gpui::Role::Button);
            assert!(disabled.is_disabled());
            assert!(!disabled.supports_action(gpui::accesskit::Action::Click));
            window.click("status-item-project-up", app)
        });
        test_turn(cx, window, |window, app| {
            assert!(owner.read(app).changes.is_empty());
            window.click("status-item-live-sessions-down", app);
        });
        test_turn(cx, window, |window, app| {
            assert_eq!(owner.read(app).changes.len(), 1);
            assert_ne!(
                owner.read(app).settings.items,
                StatusBarSettings::default().items
            );
            // Pointer activation and a subsequent Base Enter each produce one reset.
            window.click("status-bar-reset", app);
        });
        test_turn(cx, window, |window, app| window.press("enter", app));
        test_turn(cx, window, |_window, app| {
            assert_eq!(owner.read(app).changes.len(), 3);
            assert_eq!(owner.read(app).settings, StatusBarSettings::default());
        });
    }
}
