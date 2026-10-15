//! A project window's side of the global control: the status bar item and the menu above it,
//! whose Dashboard and Chat rows bring up the global window (`global_window`). The dashboard
//! and the global chat are never a project window's tabs. The menu says what each row shows
//! or what a click does from what this window samples anyway; opening or drawing it starts
//! nothing, and only the Chat row may start an orchestrator, as Settings say.

use super::*;

/// What a row of the global menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlobalMenuAction {
    Dashboard,
    Chat,
}

impl Workspace {
    /// Every project's work, from what the window last sampled.
    pub(super) fn dashboard_overview(&self) -> global_overview::Overview {
        global_overview::build(&global_overview::Inputs {
            state: &self.state,
            shells: &self.shells,
            cwds: &self.shell_cwds,
            activity: &self.agent_activity,
            chats: self.shared_chats.as_deref(),
            chat_activity: &self.dashboard.chat_activity,
            schedules: self.dashboard.schedules.as_ref(),
            now: activity::unix_now(),
        })
    }

    /// Read the automations and when failed or waiting chats last wrote, off the window, for
    /// the menu's Dashboard row: at once with `force`, else when the last read is
    /// `DASHBOARD_READ_EVERY` old.
    pub(super) fn refresh_dashboard(&mut self, force: bool, cx: &mut Context<Self>) {
        if !self.dashboard.due(force) {
            return;
        }
        self.dashboard.pending = true;
        let home = self.sessions.state_home().to_path_buf();
        let chats = global_window::timed_chats(self.shared_chats.as_deref());
        let work = cx
            .background_executor()
            .spawn(async move { global_window::read_extras(&home, chats) });
        cx.spawn(async move |this, cx| {
            let (schedules, written) = work.await;
            let _ = this.update(cx, |workspace, cx| {
                if workspace.dashboard.finish(schedules, written) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // ---- The status bar item and its menu ------------------------------------------------

    /// The global item: a globe under Native and G·ORCH in the colorful themes, behind the
    /// global orchestrator's activity mark. A click opens the menu above it, another closes it.
    /// Its rows open the global window.
    pub(super) fn render_global_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let open = self.global_menu_open;
        let native = ui_text::is_native();
        let color = if open {
            colors.cyan
        } else {
            status_accent(colors.magenta, colors)
        };
        let bounds = self.global_item_bounds.clone();
        let mark =
            orchestrator_mark(self.chat_orchestrator_activity(&OrchestratorScope::Global, cx));
        behavior_controls::action("top-orchestrator", "Global: dashboard and chat", colors)
            .aria_expanded(open)
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .gap(ui_text::space(4.0))
            .px(ui_text::space(4.0))
            .py(ui_text::space(1.0))
            .text_color(rgb(color))
            .when(open, |item| item.bg(rgb(colors.panel_active)))
            .hover(|style| {
                style
                    .bg(rgb(colors.panel_active))
                    .text_color(rgb(colors.cyan))
            })
            .when(native, |item| {
                item.rounded_full()
                    .px(ui_text::space(6.0))
                    .children((!mark.is_empty()).then(|| div().child(mark.trim_end())))
                    .child(icons::text_icon(
                        Icon::Panel(PanelKind::Dashboard),
                        9.0,
                        color,
                    ))
                    .children((!self.settings.panel_tab_icons).then(|| div().child("Global")))
            })
            .when(!native, |item| {
                item.child(ui_text::quiet(format!("{mark}G·ORCH")))
            })
            .child(
                canvas(move |area, _, _| bounds.set(area), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .when(!open, |item| {
                item.child(tooltip::anchor(
                    "Global · dashboard ⌘⌥G · chat ⌘⇧O",
                    Look::Status,
                ))
            })
            .on_click(cx.listener(|workspace, _, window, cx| {
                workspace.toggle_global_menu(window, cx);
            }))
            .into_any_element()
    }

    /// Whether the status bar shows the global item, which the global menu hangs from.
    pub(super) fn global_item_shown(&self) -> bool {
        use status_bar::{StatusItemKind, StatusSide};
        [StatusSide::Left, StatusSide::Right]
            .into_iter()
            .any(|side| {
                self.settings
                    .status_bar
                    .visible_items(side)
                    .contains(&StatusItemKind::GlobalOrchestrator)
            })
    }

    pub(super) fn toggle_global_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.global_menu_open {
            self.close_global_menu(window, cx);
            return;
        }
        if self.modal_open() {
            return;
        }
        self.dismiss_project_sort_menus(window, cx);
        if self.panel_menu.is_none() && !self.layout_menu_open && !self.account_menu_open {
            self.menu_return.capture(window, cx);
        }
        self.panel_menu = None;
        self.layout_menu_open = false;
        self.account_menu_open = false;
        self.global_menu_open = true;
        // Terminals are native views drawn above everything of ours; the menu is drawn over
        // their snapshots instead, as the layout menu is.
        if !self.tab_dragging {
            self.begin_tab_drag(cx);
        }
        self.menu_focus.focus(window, cx);
        // Its Chat row says what a click does; the automations feed the Dashboard row.
        self.refresh_dashboard(false, cx);
        cx.notify();
    }

    pub(super) fn close_global_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.global_menu_open {
            return;
        }
        self.global_menu_open = false;
        self.finish_tab_drag(cx);
        if !self.menu_return.restore_within(&self.focus, window, cx) {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// Close the menu for a row's own action, which decides where focus goes.
    fn dismiss_global_menu(&mut self, cx: &mut Context<Self>) {
        if self.global_menu_open {
            self.global_menu_open = false;
            self.menu_return.forget();
            self.finish_tab_drag(cx);
        }
    }

    /// The global menu, above the status bar at the global item: Dashboard and Chat, each
    /// saying what it shows or what a click does.
    pub(super) fn render_global_menu(
        &self,
        window_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let width = ui_text::space_f32(300.0).min((window_width - 12.0).max(0.0));
        let item = self.global_item_bounds.get();
        let left = layout_menu_left(
            item.right().as_f32(),
            item.size.width.as_f32(),
            width,
            window_width,
        );
        let overview = self.dashboard_overview();
        let chat = dashboard::chat_copy(
            &overview.global,
            &global_window::orchestrator_runs(&self.settings),
            global_window::chat_starting(cx),
        );
        let shown = global_window::shown_view(cx);
        let dashboard_row = self.global_menu_row(
            "global-menu-dashboard",
            Icon::Panel(PanelKind::Dashboard),
            "Dashboard".to_owned(),
            "⌘⌥G",
            dashboard::summary(&overview, self.sessions_sampled_at.is_some()),
            None,
            shown == Some(GlobalView::Dashboard),
            Some(GlobalMenuAction::Dashboard),
            cx,
        );
        let chat_row = self.global_menu_row(
            "global-menu-chat",
            chat.provider
                .map_or(Icon::Panel(PanelKind::Shells), Icon::Provider),
            chat.action.clone(),
            "⌘⇧O",
            chat.status.clone(),
            chat.note.clone(),
            shown == Some(GlobalView::Chat),
            chat.enabled.then_some(GlobalMenuAction::Chat),
            cx,
        );
        behavior_controls::focus_scope(
            div()
                .id("global-menu")
                .role(gpui::Role::Menu)
                .aria_label("Global menu")
                .absolute()
                .left(px(left))
                .bottom(ui_text::space(STATUS_BAR_HEIGHT) + px(3.0))
                .w(px(width))
                .bg(rgb(colors.panel_active))
                .border_1()
                .border_color(rgb(colors.magenta))
                .p(ui_text::space(3.0))
                .map(|menu| controls::native(menu, |menu| controls::menu(menu, colors)))
                .occlude()
                .on_mouse_down_out(cx.listener(
                    |workspace, event: &gpui::MouseDownEvent, window, cx| {
                        // A click on the item itself is its own toggle.
                        if !workspace.global_item_bounds.get().contains(&event.position) {
                            workspace.close_global_menu(window, cx);
                        }
                    },
                ))
                .child(pane_menu_heading("Global", true, colors))
                .child(dashboard_row)
                .child(chat_row),
            "global-menu-scope",
            &self.menu_focus,
        )
        .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn global_menu_row(
        &self,
        id: &'static str,
        icon: Icon,
        label: String,
        shortcut: &'static str,
        detail: String,
        note: Option<String>,
        shown: bool,
        action: Option<GlobalMenuAction>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let enabled = action.is_some();
        let small = |text: String, color: u32| {
            div()
                .pl(ui_text::space(22.0))
                .text_size(ui_text::text(9.0))
                .text_color(rgb(color))
                .child(text)
        };
        behavior_controls::action(
            id,
            format!("{label}{}: {detail}", if shown { ", shown" } else { "" }),
            colors,
        )
        .role(gpui::Role::MenuItem)
        .disabled(!enabled)
        .flex()
        .flex_col()
        .gap(ui_text::space(2.0))
        .px(ui_text::space(8.0))
        .py(ui_text::space(5.0))
        .text_size(ui_text::text(10.0))
        .map(|item| controls::native(item, |item| controls::menu_row(item, colors)))
        .text_color(rgb(if enabled { colors.text } else { colors.muted }))
        .when(enabled, |item| {
            item.hover(move |style| {
                controls::hovered(style, controls::menu_row_hover(colors), |style| {
                    style.bg(rgb(colors.divider)).text_color(rgb(colors.cyan))
                })
            })
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(ui_text::space(8.0))
                .child(
                    div()
                        .w(ui_text::space(14.0))
                        .flex_none()
                        .child(menu_icon(icon, colors)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(if ui_text::is_native() {
                            gpui::SharedString::from(label)
                        } else {
                            ui_text::cased(label)
                        }),
                )
                .children(shown.then(|| menu_icon(Icon::Check, colors)))
                .child(
                    div()
                        .flex_none()
                        .text_size(ui_text::text(9.0))
                        .text_color(rgb(colors.muted))
                        .child(shortcut),
                ),
        )
        .child(small(detail, colors.muted))
        .children(note.map(|note| small(note, colors.muted)))
        .when_some(action, |item, action| {
            item.on_click(cx.listener(move |workspace, _, window, cx| {
                workspace.dismiss_global_menu(cx);
                // The global window takes focus; this one gets it back when it is raised.
                workspace.focus_active(window, cx);
                match action {
                    GlobalMenuAction::Dashboard => {
                        global_window::open(GlobalView::Dashboard, false, cx)
                    }
                    GlobalMenuAction::Chat => global_window::open(GlobalView::Chat, true, cx),
                }
                cx.notify();
            }))
        })
        .into_any_element()
    }
}

#[cfg(test)]
mod tests;
