//! The window's side of the global control: the status bar item, the menu above it with the
//! dashboard and the chat, and what the dashboard's clicks do.
//!
//! The dashboard opens as a panel tab (`PanelKind::Dashboard`), one per window: opening it
//! again selects the tab there is. It is saved with the layout like any panel. Chat is the
//! global orchestrator, reached the way G·ORCH always reached it, so it keeps its identity,
//! its provider, permission and model controls, its transcript and its draft, and a terminal
//! orchestrator stays a terminal. Opening the dashboard or the menu, drawing them and
//! refreshing them start nothing; only clicking Chat may start an orchestrator, and then only
//! as Settings say (see `orchestrators::create`).

use super::*;
use dashboard::{DashboardAction, DashboardData};
use global_overview::Target;

/// What a row of the global menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlobalMenuAction {
    Dashboard,
    Chat,
}

/// The pane a new dashboard tab opens in: the main pane, else the selected pane, else any pane
/// that is not locked, as a new chat from Sessions goes; the selected one when every pane is
/// locked. A dashboard that is already open is selected where it is instead (`open_panel`).
pub(crate) fn dashboard_pane(
    panes: impl IntoIterator<Item = PaneId>,
    main: Option<PaneId>,
    selected: PaneId,
    locked: &dyn Fn(PaneId) -> bool,
) -> PaneId {
    chat_choice::destination(panes, main, selected, locked).unwrap_or(selected)
}

impl Workspace {
    pub(super) fn open_global_dashboard_action(
        &mut self,
        _: &OpenGlobalDashboard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_dashboard(window, cx);
    }

    /// Show the dashboard: its tab where it is, or a new one where `dashboard_pane` says.
    /// Nothing is started; the automations are read again at once.
    pub(super) fn open_dashboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_global_menu(cx);
        if self.modal_open() || !self.ensure_layout(window, cx) {
            return;
        }
        let pane = dashboard_pane(
            self.panes.keys().copied(),
            self.main_pane(),
            self.active_pane,
            &|pane| self.pane_is_locked(pane),
        );
        self.open_panel(PanelKind::Dashboard, pane, window, cx);
        self.refresh_dashboard(true, cx);
        cx.notify();
    }

    /// How a new orchestrator would run, as Settings say now.
    fn orchestrator_runs(&self) -> settings::OrchestratorRuns {
        match self.settings.orchestrator_mode {
            settings::OrchestratorMode::Terminal => settings::OrchestratorRuns::Terminal,
            settings::OrchestratorMode::Chat => {
                settings::OrchestratorRuns::Chat(self.settings.orchestrator_chat_provider)
            }
        }
    }

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

    pub(super) fn render_dashboard(&self, cx: &mut Context<Self>) -> AnyElement {
        let overview = self.dashboard_overview();
        let host = self.remote_host_name(cx);
        dashboard::render(
            DashboardData {
                overview: &overview,
                now: activity::unix_now(),
                sampled_at: self.sessions_sampled_at,
                sample_error: self.sessions_sample_error.as_deref(),
                chat_starting: self
                    .orchestrators_starting
                    .contains(&OrchestratorScope::Global),
                runs: self.orchestrator_runs(),
                remote_host: host.as_deref(),
                show_quiet: self.dashboard.show_quiet,
            },
            Self::dashboard_action,
            cx,
        )
    }

    /// Read the automations and when failed or waiting chats last wrote, off the window: at
    /// once with `force`, else when the last read is `DASHBOARD_READ_EVERY` old.
    pub(super) fn refresh_dashboard(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.dashboard.pending
            || (!force
                && self
                    .dashboard
                    .read_at
                    .is_some_and(|at| at.elapsed() < DASHBOARD_READ_EVERY))
        {
            return;
        }
        self.dashboard.pending = true;
        let home = self.sessions.state_home().to_path_buf();
        // Only these need a time to be told apart from history; a few hundred at most.
        let chats: Vec<String> = self
            .shared_chats
            .iter()
            .flatten()
            .filter(|chat| {
                matches!(
                    chat.state,
                    chat::model::ChatState::Failed { .. } | chat::model::ChatState::Waiting
                )
            })
            .map(|chat| chat.id.clone())
            .take(256)
            .collect();
        let work = cx.background_executor().spawn(async move {
            let schedules =
                schedules::ScheduleStore::at(home.clone()).and_then(|store| store.list());
            let written = chats
                .into_iter()
                .filter_map(|id| {
                    let log = chat::log::chat_dir(&home, &id)?.join("events.jsonl");
                    let at = std::fs::metadata(log)
                        .ok()?
                        .modified()
                        .ok()?
                        .duration_since(UNIX_EPOCH)
                        .ok()?
                        .as_secs();
                    Some((id, at))
                })
                .collect::<BTreeMap<_, _>>();
            (schedules, written)
        });
        cx.spawn(async move |this, cx| {
            let (schedules, written) = work.await;
            let _ = this.update(cx, |workspace, cx| {
                let reads = &mut workspace.dashboard;
                reads.pending = false;
                reads.read_at = Some(Instant::now());
                let changed =
                    reads.schedules.as_ref() != Some(&schedules) || reads.chat_activity != written;
                reads.schedules = Some(schedules);
                reads.chat_activity = written;
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Whether a dashboard tab is the selected tab of a pane on screen.
    pub(super) fn dashboard_on_screen(&self) -> bool {
        self.panes.iter().any(|(pane_id, pane)| {
            self.tab_is_shown(*pane_id, pane.active, pane.active)
                && pane
                    .tabs
                    .get(pane.active)
                    .is_some_and(|tab| tab.panel() == Some(PanelKind::Dashboard))
        })
    }

    /// Whether the global orchestrator's tab, chat or terminal, is the selected tab of a pane.
    fn global_chat_on_screen(&self) -> bool {
        let chat = self
            .chat_orchestrators
            .iter()
            .flatten()
            .find(|chat| chat.orchestrator == Some(OrchestratorScope::Global))
            .map(|chat| chat.id.as_str());
        let terminal = self
            .shells
            .iter()
            .find(|shell| shell.kind == ShellKind::Orchestrator && shell.project_id.is_none())
            .map(|shell| shell.id.as_str());
        self.panes.values().any(|pane| {
            pane.tabs.get(pane.active).is_some_and(|tab| {
                (chat.is_some() && tab.chat_id() == chat)
                    || (terminal.is_some() && tab.shell_id() == terminal)
            })
        })
    }

    /// A click on the dashboard.
    pub(super) fn dashboard_action(
        &mut self,
        action: DashboardAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        match action {
            DashboardAction::Refresh => {
                self.refresh_dashboard(true, cx);
                self.refresh(window, cx);
                cx.notify();
            }
            DashboardAction::ToggleQuietProjects => {
                self.dashboard.show_quiet = !self.dashboard.show_quiet;
                cx.notify();
            }
            DashboardAction::GlobalChat => self.open_orchestrator(window, cx),
            DashboardAction::ProjectPanel { project_id, panel } => {
                if self.switch_project(&project_id, window, cx) {
                    self.open_panel(panel, self.active_pane, window, cx);
                }
            }
            DashboardAction::Open(target) => self.open_dashboard_target(target, window, cx),
        }
    }

    /// Make `project_id` the window's project, if it is not; whether it is now.
    fn switch_project(
        &mut self,
        project_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.project_id != project_id {
            self.activate_project(project_id, true, window, cx);
        }
        self.project_id == project_id
    }

    /// Something of the global scope opens among this Mac's own tabs: a window that shows
    /// another Mac's project goes back to the last local one first. Whether it is local now.
    fn come_home(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.is_remote() {
            return true;
        }
        match self.state.active_project_id.clone() {
            Some(local) => self.switch_project(&local, window, cx),
            None => {
                self.notice = Some("Open one of this Mac's projects first.".into());
                cx.notify();
                false
            }
        }
    }

    fn open_dashboard_target(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            Target::Shell { id } => {
                let project = self
                    .shells
                    .iter()
                    .find(|shell| shell.id == id)
                    .map(|shell| shell.project_id.clone());
                match project {
                    // `show_shell` switches to the shell's project itself.
                    Some(Some(_)) => self.show_shell(&id, window, cx),
                    Some(None) => {
                        if self.come_home(window, cx) {
                            self.show_shell(&id, window, cx);
                        }
                    }
                    None => {
                        self.notice = Some("That session has ended.".into());
                        cx.notify();
                    }
                }
            }
            Target::Chat { id, .. } => self.open_dashboard_chat(&id, window, cx),
            Target::Project { id } => self.activate_project(&id, true, window, cx),
            Target::Task { id } => {
                let Some(project) = self
                    .state
                    .tasks
                    .iter()
                    .find(|task| task.id == id)
                    .map(|task| task.project_id.clone())
                else {
                    self.notice = Some("That task is gone.".into());
                    cx.notify();
                    return;
                };
                if self.switch_project(&project, window, cx) {
                    self.select_task(&id, window, cx);
                    self.open_panel(PanelKind::Tasks, self.active_pane, window, cx);
                }
            }
            Target::Automations { project_id } => {
                let local = match project_id {
                    Some(project) => self.switch_project(&project, window, cx),
                    None => self.come_home(window, cx),
                };
                if local {
                    self.open_panel(PanelKind::Schedules, self.active_pane, window, cx);
                }
            }
        }
    }

    /// Open a chat a dashboard row names, as it is: an orchestrator by the routing G·ORCH and
    /// P·ORCH use, any other chat as Sessions opens it, in its own project. Nothing is
    /// created: a chat the window no longer knows is reported gone.
    fn open_dashboard_chat(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self
            .shared_chats
            .iter()
            .flatten()
            .find(|chat| chat.id == id)
            .cloned()
        else {
            self.notice = Some("That chat is gone.".into());
            cx.notify();
            return;
        };
        let local = match chat.project_id.as_deref() {
            Some(project) => self.switch_project(project, window, cx),
            None => self.come_home(window, cx),
        };
        if !local {
            return;
        }
        if chat.orchestrator.is_some() {
            self.show_orchestrator_chat(chat, window, cx);
        } else if chat.project_id.is_some() {
            self.show_existing_chat(chat, window, cx);
        } else {
            self.notice = Some("This chat belongs to no project; open it with riwork chat.".into());
            cx.notify();
        }
    }

    // ---- The status bar item and its menu ------------------------------------------------

    /// The global item: a globe under Native and G·ORCH in the colorful themes, behind the
    /// global orchestrator's activity mark. A click opens the menu above it, another closes it.
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
        if self.panel_menu.is_none() && !self.layout_menu_open {
            self.menu_return.capture(window, cx);
        }
        self.panel_menu = None;
        self.layout_menu_open = false;
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
        let host = self.remote_host_name(cx);
        let chat = dashboard::chat_copy(
            &overview.global,
            self.orchestrator_runs(),
            self.orchestrators_starting
                .contains(&OrchestratorScope::Global),
            host.as_deref(),
        );
        let dashboard_row = self.global_menu_row(
            "global-menu-dashboard",
            Icon::Panel(PanelKind::Dashboard),
            "Dashboard".to_owned(),
            "⌘⌥G",
            dashboard::summary(&overview, self.sessions_sampled_at.is_some()),
            None,
            self.dashboard_on_screen(),
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
            self.global_chat_on_screen(),
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
                match action {
                    GlobalMenuAction::Dashboard => workspace.open_dashboard(window, cx),
                    GlobalMenuAction::Chat => workspace.open_orchestrator(window, cx),
                }
            }))
        })
        .into_any_element()
    }
}

#[cfg(test)]
mod tests;
