//! The project's one tab strip. A local project's pane bar draws the shared tab list
//! (see `project_tabs`): its built-in panels as a compact leading segment of symbols, then
//! the pinned session tabs, then the rest in shared order, scrolling sideways when they do
//! not fit, then ＋ and, when tabs are out of sight, All tabs. Each session tab is a Kit
//! button with the tab role, a status dot, its canonical title and a close mark revealed on
//! hover. Tabs are the pane bar's flat cells (`flat_cell`). Remote projects keep their own
//! pane bar.

use super::*;

/// What the strip's own menus show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StripMenuKind {
    /// ＋: new terminal and chats, and the way into the worker picker.
    New,
    /// The hidden chats and shells of the project, to open as tabs.
    Workers,
    /// Every session tab of the pane, including those scrolled out of sight.
    AllTabs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StripMenu {
    pub pane: PaneId,
    pub kind: StripMenuKind,
}

/// What a strip last showed, to tell when its selected tab needs revealing.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StripShape {
    width: i32,
    tabs: usize,
    selected: Option<TabId>,
    /// Where the selected tab sits among the scrolling tabs, and the width before it: a
    /// shared Move, a rename or a peer's update can shift it without changing the rest.
    position: Option<usize>,
    lead: i32,
    /// The selected tab's own width, and what sits fixed before the scrolling tabs (panels
    /// and pinned tabs): renaming either moves the selected tab's edges.
    own: i32,
    fixed: i32,
    /// Frames left to keep the selected tab in view after a change.
    settling: u8,
}

impl StripShape {
    fn same_place(&self, other: &Self) -> bool {
        (self.width, self.tabs, self.selected, self.position, self.lead, self.own, self.fixed)
            == (
                other.width,
                other.tabs,
                other.selected,
                other.position,
                other.lead,
                other.own,
                other.fixed,
            )
    }
}

/// Width of the strip's ＋ and All tabs cells.
const STRIP_BUTTON: f32 = 28.0;
const TITLE_MAX_WIDTH: f32 = 220.0;
const DOT: f32 = 7.0;

/// One tab of a strip, as the strip lays it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StripSlot {
    Panel,
    Pinned,
    Session,
}

/// The order the strip draws a pane's tabs in: panels in the pane's order, then pinned
/// sessions, then the rest, both in shared order (`rank`, the entry's `order`); a view
/// without a shared entry keeps its place after them. Indices into the pane's tabs.
pub(crate) fn strip_order(slots: &[(StripSlot, usize)]) -> Vec<usize> {
    [StripSlot::Panel, StripSlot::Pinned, StripSlot::Session]
        .into_iter()
        .flat_map(|group| {
            let mut members = slots
                .iter()
                .enumerate()
                .filter(|(_, (slot, _))| *slot == group)
                .map(|(index, (_, rank))| (*rank, index))
                .collect::<Vec<_>>();
            if group != StripSlot::Panel {
                members.sort();
            }
            members.into_iter().map(|(_, index)| index)
        })
        .collect()
}

/// The tab `⌘number` selects: the first eight session tabs by position, ⌘9 the last.
pub(crate) fn numbered_session(
    order: &[usize],
    slots: &[(StripSlot, usize)],
    number: usize,
) -> Option<usize> {
    let sessions = order
        .iter()
        .copied()
        .filter(|index| slots[*index].0 != StripSlot::Panel)
        .collect::<Vec<_>>();
    match number {
        9 => sessions.last().copied(),
        1..=8 => sessions.get(number - 1).copied(),
        _ => None,
    }
}

/// Whether pinned tabs `width` wide would leave the other tabs less than half of `room`;
/// they then scroll with them.
pub(crate) fn pins_scroll(width: f32, room: f32) -> bool {
    width > room / 2.0
}

/// Whether the scrolling tabs need more than `room`: the strip then offers All tabs.
pub(crate) fn strip_overflows(widths: &[f32], gap: f32, room: f32) -> bool {
    let total: f32 = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
    total > room
}

/// The word VoiceOver and the picker say for a status.
pub(crate) fn status_word(status: &project_tabs::Status) -> &'static str {
    match status {
        project_tabs::Status::Working => "working",
        project_tabs::Status::Waiting => "waiting",
        project_tabs::Status::Error => "error",
        project_tabs::Status::Done => "done",
        project_tabs::Status::Stopped => "stopped",
    }
}

/// A status from what a tab's agent is doing, for a tab without a shared entry (the global
/// orchestrator's view, a view carried in from another window before its list arrives).
fn activity_status(activity: AgentActivity) -> Option<project_tabs::Status> {
    match activity {
        AgentActivity::Working => Some(project_tabs::Status::Working),
        AgentActivity::Waiting => Some(project_tabs::Status::Waiting),
        AgentActivity::Done => Some(project_tabs::Status::Done),
        AgentActivity::Exited => Some(project_tabs::Status::Stopped),
        AgentActivity::Unknown => None,
    }
}

/// The phone's colours: working in the theme's working accent, waiting in gold, an error
/// in the terminal's red, done muted; a stopped session is an empty ring, so the state is
/// never told by colour alone.
pub(crate) fn status_dot(status: &project_tabs::Status, colors: Palette, error: u32) -> AnyElement {
    let dot = div().flex_none().size(ui_text::space(DOT)).rounded_full();
    match status {
        project_tabs::Status::Working => dot.bg(rgb(colors.working)),
        project_tabs::Status::Waiting => dot.bg(rgb(colors.gold)),
        project_tabs::Status::Error => dot.bg(rgb(error)),
        project_tabs::Status::Done => dot.bg(rgb(theme::mix(colors.muted, colors.panel, 0.25))),
        project_tabs::Status::Stopped => dot.border_1().border_color(rgb(colors.muted)),
    }
    .into_any_element()
}

/// A tab cell as the pane bar draws it: full height, square, a hairline after it; the
/// selected cell filled, in the content's background in the selected pane.
pub(crate) fn flat_cell<E: Styled>(
    cell: E,
    active: bool,
    pane_selected: bool,
    colors: Palette,
) -> E {
    if !colors.plain_tabs {
        return cell
            .border_r_1()
            .border_b_1()
            .border_color(rgb(if active { colors.cyan } else { colors.divider }))
            .bg(rgb(if active {
                colors.panel_active
            } else {
                colors.panel
            }));
    }
    let cell = cell.border_r_1().border_color(rgb(colors.divider));
    if active {
        cell.bg(rgb(if pane_selected {
            colors.bg
        } else {
            colors.panel_active
        }))
        .font_weight(gpui::FontWeight::MEDIUM)
    } else {
        cell
    }
}

/// The `before` of a Move that takes the tab drawn at `from` to where the tab at `to` is:
/// the next tab of its pin group after that spot, or `None` for the group's end. `None`
/// overall when the spot is in the other pin group, or nothing moves. Parents do not part
/// the strip: a tab goes beside another tab's workers as beside any other tab.
pub(crate) fn strip_move(
    entries: &[project_tabs::Entry],
    drawn: &[(TabId, String)],
    from: usize,
    to: usize,
) -> Option<Option<String>> {
    if from == to {
        return None;
    }
    let entry = |key: &str| entries.iter().find(|e| e.key == key);
    let moved = entry(&drawn[from].1)?;
    let same = |key: &str| entry(key).is_some_and(|e| e.pinned == moved.pinned);
    if !same(&drawn[to].1) {
        return None;
    }
    let after = if to > from { to + 1 } else { to };
    Some(
        drawn[after..]
            .iter()
            .map(|(_, key)| key)
            .find(|key| **key != drawn[from].1 && same(key))
            .cloned(),
    )
}

impl Workspace {
    /// A local project with a shared list draws the one strip; remote projects and a
    /// window whose list is unavailable keep the pane bar.
    pub(crate) fn uses_tab_strip(&self) -> bool {
        self.shared_tabs.is_some() && !self.is_remote()
    }

    /// The shared entry behind a session key: this project's, or a carried foreign view's.
    pub(crate) fn strip_entry(&self, key: &str) -> Option<&project_tabs::Entry> {
        self.shared_tab_entries()
            .iter()
            .find(|e| e.key == key)
            .or_else(|| self.foreign_tabs.get(key).and_then(Option::as_ref))
    }

    /// Each tab's group and shared rank.
    pub(crate) fn strip_slots(&self, pane: &Pane) -> Vec<(StripSlot, usize)> {
        pane.tabs
            .iter()
            .map(|tab| {
                if tab.panel().is_some() {
                    return (StripSlot::Panel, 0);
                }
                // Carried foreign views rank after this project's tabs.
                let shared = session_tab_key(tab)
                    .and_then(|key| self.shared_tab_entries().iter().find(|e| e.key == key));
                let pinned = session_tab_key(tab)
                    .and_then(|key| self.strip_entry(&key))
                    .is_some_and(|e| e.pinned);
                (
                    if pinned {
                        StripSlot::Pinned
                    } else {
                        StripSlot::Session
                    },
                    shared.map_or(usize::MAX, |e| e.order),
                )
            })
            .collect()
    }

    /// The order ⌘⇧] / Ctrl+Tab walk and ⌘1–9 count in: the strip's, or the pane's own
    /// where there is no strip.
    pub(crate) fn tab_cycle_order(&self, pane: &Pane) -> Vec<usize> {
        if self.uses_tab_strip() {
            strip_order(&self.strip_slots(pane))
        } else {
            (0..pane.tabs.len()).collect()
        }
    }

    pub(crate) fn select_numbered_tab(
        &mut self,
        number: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        let Some(pane) = self.panes.get(&self.active_pane) else {
            return;
        };
        let slots = if self.uses_tab_strip() {
            self.strip_slots(pane)
        } else {
            vec![(StripSlot::Session, 0); pane.tabs.len()]
        };
        let order = self.tab_cycle_order(pane);
        if let Some(index) = numbered_session(&order, &slots, number) {
            let (pane_id, tab_id) = (self.active_pane, pane.tabs[index].id);
            self.select_tab(pane_id, tab_id, window, cx);
        }
    }

    /// A tab dropped on a strip: placement is local (`move_tab`); a reorder within the same
    /// strip also publishes Move, kept to the dragged tab's pin group, so the
    /// phone sees the new order. Dropped on `target` it lands where that tab was, before it
    /// when dragged leftward and after it when dragged rightward; `None` is the strip's end.
    pub(crate) fn strip_drop(
        &mut self,
        drag: &DraggedTab,
        pane_id: PaneId,
        index: usize,
        target: Option<TabId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reorder = (drag.pane_id == pane_id && drag.project_id == self.project_id)
            .then(|| {
                let pane = self.panes.get(&pane_id)?;
                let slots = self.strip_slots(pane);
                let drawn = strip_order(&slots)
                    .into_iter()
                    .filter(|i| slots[*i].0 != StripSlot::Panel)
                    .filter_map(|i| Some((pane.tabs[i].id, session_tab_key(&pane.tabs[i])?)))
                    .collect::<Vec<_>>();
                let from = drawn.iter().position(|(id, _)| *id == drag.tab_id)?;
                let key = drawn[from].1.clone();
                let to = match target {
                    Some(target) => drawn.iter().position(|(id, _)| *id == target)?,
                    None => drawn.len() - 1,
                };
                strip_move(self.shared_tab_entries(), &drawn, from, to)
                    .map(|before| project_tabs::Update::Move { key, before })
            })
            .flatten();
        self.move_tab(drag, pane_id, index, window, cx);
        if let Some(update) = reorder
            && let Err(error) = self.change_shared_tab(&update, window, cx)
        {
            self.report_tab_store_error(error);
        }
    }

    /// Bring a selected tab into the visible part of its strip.
    pub(crate) fn reveal_strip_tab(&self, pane_id: PaneId, tab_id: TabId) {
        let Some(pane) = self.panes.get(&pane_id) else {
            return;
        };
        let slots = self.strip_slots(pane);
        let scrolls = self.strip_scrolls.borrow();
        let Some((handle, pins_scroll, _)) = scrolls.get(&pane_id) else {
            return;
        };
        let scrolled = strip_order(&slots)
            .into_iter()
            .filter(|index| match slots[*index].0 {
                StripSlot::Panel => false,
                StripSlot::Pinned => *pins_scroll,
                StripSlot::Session => true,
            })
            .position(|index| pane.tabs[index].id == tab_id);
        if let Some(index) = scrolled {
            handle.scroll_to_item(index);
        }
    }

    fn strip_title(&self, tab: &Tab, cx: &App) -> String {
        if let Some(entry) = session_tab_key(tab).and_then(|key| self.strip_entry(&key)) {
            return entry.title.clone();
        }
        match tab.chat() {
            Some(view) => orchestrators::shown_tab_title(&view.read(cx).summary().title),
            None => orchestrators::shown_tab_title(&tab.title),
        }
    }

    fn strip_status(&self, tab: &Tab, cx: &App) -> Option<project_tabs::Status> {
        if let Some(entry) = session_tab_key(tab).and_then(|key| self.strip_entry(&key)) {
            return Some(entry.status.clone());
        }
        let activity = match tab.chat() {
            Some(view) => view.read(cx).summary().activity,
            None => tab
                .shell_id()
                .and_then(|id| self.agent_activity.get(id))
                .map(|state| state.activity),
        };
        activity.and_then(activity_status)
    }

    pub(crate) fn toggle_strip_menu(
        &mut self,
        pane: PaneId,
        kind: StripMenuKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        let menu = StripMenu { pane, kind };
        if self.strip_menu == Some(menu) {
            self.close_strip_menu(window, cx);
            return;
        }
        self.dismiss_project_sort_menus(window, cx);
        if self.strip_menu.is_none() && self.panel_menu.is_none() {
            self.menu_return.capture(window, cx);
        }
        self.panel_menu = None;
        self.layout_menu_open = false;
        self.active_pane = pane;
        self.strip_menu = Some(menu);
        self.strip_menu_selection = self.selection();
        self.begin_tab_drag(cx);
        self.menu_focus.focus(window, cx);
        // The Kit popup places itself after measuring; draw the frame that shows it.
        cx.on_next_frame(window, |_, _, cx| cx.notify());
        cx.notify();
    }

    /// The selected pane and its selected tab.
    pub(crate) fn selection(&self) -> (PaneId, Option<TabId>) {
        let tab = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
            .map(|tab| tab.id);
        (self.active_pane, tab)
    }

    /// A strip menu goes when its strip does (its pane closed, focus mode, a project
    /// switch) and when something else acted while it was open: a shortcut changed the
    /// selection, or thawed the terminals it froze.
    pub(crate) fn settle_strip_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.strip_menu else {
            return;
        };
        let gone =
            self.focus_mode || !self.uses_tab_strip() || !self.panes.contains_key(&menu.pane);
        let moved = self.selection() != self.strip_menu_selection;
        if gone || moved || !self.tab_dragging {
            self.strip_menu = None;
            self.finish_tab_drag(cx);
            if moved || gone {
                self.focus_active(window, cx);
            }
        }
    }

    pub(crate) fn close_strip_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.strip_menu.take().is_none() {
            return;
        }
        self.finish_tab_drag(cx);
        if !self.menu_return.restore_within(&self.focus, window, cx) {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// The pane bar of a local project: the one tab strip. `room` is what the bar has for
    /// its tabs and buttons beside the pane buttons (`bar_buttons`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_tab_strip(
        &self,
        pane_id: PaneId,
        room: f32,
        control_inset: f32,
        window_drag: bool,
        drag_handle: Option<f32>,
        drag_room: bool,
        tab_can_close: bool,
        bar_buttons: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let native = ui_text::is_native();
        let Some(pane) = self.panes.get(&pane_id) else {
            return div().into_any_element();
        };
        let pane_selected = self.active_pane == pane_id;
        let slots = self.strip_slots(pane);
        let order = strip_order(&slots);
        let in_group = |group: StripSlot| {
            order
                .iter()
                .copied()
                .filter(|index| slots[*index].0 == group)
                .collect::<Vec<_>>()
        };
        let (panels, pinned, sessions) = (
            in_group(StripSlot::Panel),
            in_group(StripSlot::Pinned),
            in_group(StripSlot::Session),
        );
        // Cells sit edge to edge, each with its hairlines.
        let gap = 2.0;
        let width = |index: usize| {
            let tab = &pane.tabs[index];
            let closable = tab_can_close && slots[index].0 != StripSlot::Pinned;
            ui_text::space_f32(8.0 + DOT + 6.0)
                + native_label_width(&self.strip_title(tab, cx), cx)
                    .min(ui_text::space_f32(TITLE_MAX_WIDTH))
                + ui_text::space_f32(if closable { 6.0 + 16.0 + 4.0 } else { 8.0 })
                + if slots[index].0 == StripSlot::Pinned {
                    ui_text::space_f32(8.0 + 6.0)
                } else {
                    0.0
                }
        };
        let base = panels.len() as f32 * (ui_text::space_f32(NATIVE_ICON_TAB_FULL) + gap)
            + 2.0 * (ui_text::space_f32(STRIP_BUTTON) + gap);
        let pinned_width = pinned.iter().map(|i| width(*i) + gap).sum::<f32>();
        // Pinned tabs stay put while they leave the others at least half the room; in a
        // pane too narrow for that they scroll too, still first, so ＋ and All tabs stay.
        let pins_scroll = pins_scroll(pinned_width, room - base);
        let (pinned, sessions) = if pins_scroll {
            (
                Vec::new(),
                pinned.into_iter().chain(sessions).collect::<Vec<_>>(),
            )
        } else {
            (pinned, sessions)
        };
        let fixed = base + if pins_scroll { 0.0 } else { pinned_width };
        let all_open = self.strip_menu
            == Some(StripMenu {
                pane: pane_id,
                kind: StripMenuKind::AllTabs,
            });
        // An open All tabs menu keeps its button, even once the window has grown to fit.
        let overflow = all_open
            || strip_overflows(
                &sessions.iter().map(|i| width(*i)).collect::<Vec<_>>(),
                gap,
                room - fixed,
            );
        let scroll = {
            let mut scrolls = self.strip_scrolls.borrow_mut();
            let entry = scrolls.entry(pane_id).or_default();
            entry.1 = pins_scroll;
            // The selected tab comes into view when the strip changes width or contents,
            // or another tab is selected; otherwise the strip stays where it was scrolled.
            let position = sessions.iter().position(|index| *index == pane.active);
            let shape = StripShape {
                width: room.round() as i32,
                tabs: sessions.len(),
                selected: pane.tabs.get(pane.active).map(|tab| tab.id),
                position,
                lead: position.map_or(0, |at| {
                    sessions[..at].iter().map(|i| width(*i)).sum::<f32>().round() as i32
                }),
                own: position.map_or(0, |at| width(sessions[at]).round() as i32),
                fixed: fixed.round() as i32,
                settling: 0,
            };
            // GPUI measures against the previous frame, so the request is repeated for the
            // frames that settle the strip's layout after a change.
            if !entry.2.same_place(&shape) {
                entry.2 = StripShape {
                    settling: 3,
                    ..shape
                };
            }
            if entry.2.settling > 0 {
                entry.2.settling -= 1;
                if let Some(at) = sessions.iter().position(|index| *index == pane.active) {
                    entry.0.scroll_to_item(at);
                }
                cx.on_next_frame(window, |_, _, cx| cx.notify());
            }
            entry.0.clone()
        };
        let error = theme::diff_colors(cx).removed;
        let tab = |index: usize, cx: &mut Context<Self>| {
            self.strip_tab(
                pane_id,
                index,
                slots[index].0,
                pane_selected,
                tab_can_close,
                error,
                cx,
            )
        };
        let panel_segment = (!panels.is_empty()).then(|| {
            behavior_controls::segments(("strip-panels", pane_id), "Panels", colors)
                .flex_none()
                .h_full()
                .items_center()
                .gap(px(0.0))
                .p(px(0.0))
                .rounded(px(0.0))
                .bg(gpui::transparent_black())
                .children(
                    panels
                        .iter()
                        .map(|index| self.strip_panel(pane_id, *index, cx)),
                )
                .into_any_element()
        });
        let new_open = self.strip_menu
            == Some(StripMenu {
                pane: pane_id,
                kind: StripMenuKind::New,
            })
            || self.strip_menu
                == Some(StripMenu {
                    pane: pane_id,
                    kind: StripMenuKind::Workers,
                });
        let plus = behavior_controls::popup(
            ("strip-new-popup", pane_id),
            self.strip_button(
                ("strip-new", pane_id),
                "New tab or open a worker",
                icons::text_icon(Icon::Add, 10.0, colors.muted),
                new_open,
                cx,
            )
            .on_click(cx.listener(move |workspace, _, window, cx| {
                workspace.toggle_strip_menu(pane_id, StripMenuKind::New, window, cx);
            })),
        )
        .anchor(gpui::Anchor::TopLeft)
        .when(new_open, |popup| {
            popup.content(self.render_strip_menu(pane_id, cx))
        });
        let all_tabs = overflow.then(|| {
            behavior_controls::popup(
                ("strip-all-popup", pane_id),
                self.strip_button(
                    ("strip-all", pane_id),
                    "All tabs",
                    if native {
                        icons::symbol("chevron.down", 9.0, None)
                    } else {
                        div().child("⌄").into_any_element()
                    },
                    all_open,
                    cx,
                )
                .on_click(cx.listener(move |workspace, _, window, cx| {
                    workspace.toggle_strip_menu(pane_id, StripMenuKind::AllTabs, window, cx);
                })),
            )
            .anchor(gpui::Anchor::TopLeft)
            .when(all_open, |popup| {
                popup.content(self.render_strip_menu(pane_id, cx))
            })
        });
        let append = move |workspace: &mut Self,
                           drag: &DraggedTab,
                           window: &mut Window,
                           cx: &mut Context<Self>| {
            let len = workspace
                .panes
                .get(&pane_id)
                .map(|pane| pane.tabs.len())
                .unwrap_or(0);
            workspace.strip_drop(drag, pane_id, len, None, window, cx);
            cx.stop_propagation();
        };
        div()
            .id(("pane-header", pane_id))
            .overflow_hidden()
            .h(ui_text::space(PANE_HEADER_HEIGHT))
            .flex_none()
            .flex()
            .min_w_0()
            .items_center()
            .bg(rgb(colors.panel))
            .border_b_1()
            .border_color(rgb(if pane_selected && !colors.plain_tabs {
                colors.cyan
            } else {
                colors.divider
            }))
            .pl(px(control_inset))
            .child(
                div()
                    .id(("tab-strip", pane_id))
                    .role(gpui::Role::TabList)
                    .aria_label("Tabs")
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .children(panel_segment)
                    .children(pinned.iter().map(|index| tab(*index, cx)))
                    .child(
                        div()
                            .id(("tab-strip-scroll", pane_id))
                            .flex()
                            .flex_shrink(1.0)
                            .min_w_0()
                            .h_full()
                            .items_center()
                            .overflow_x_scroll()
                            .track_scroll(&scroll)
                            .children(sessions.iter().map(|index| tab(*index, cx)))
                            .on_drop(cx.listener(append)),
                    )
                    .child(plus)
                    .children(all_tabs)
                    .child(
                        div()
                            .id(("window-drag-space", pane_id))
                            .flex_1()
                            .min_w(ui_text::space(if drag_room { 18.0 } else { 0.0 }))
                            .h_full()
                            .when(window_drag, |space| {
                                space.on_mouse_down(MouseButton::Left, start_window_drag)
                            })
                            .on_drop(cx.listener(append)),
                    ),
            )
            .children(drag_handle.map(|width| {
                div()
                    .id(("window-drag-handle", pane_id))
                    .flex_none()
                    .w(px(width))
                    .h_full()
                    .on_mouse_down(MouseButton::Left, start_window_drag)
            }))
            .child(bar_buttons)
            .into_any_element()
    }

    /// A symbol cell of the strip, flat like its tabs.
    fn strip_button(
        &self,
        id: impl Into<gpui::ElementId>,
        name: &'static str,
        icon: AnyElement,
        open: bool,
        cx: &mut Context<Self>,
    ) -> behavior_controls::Button {
        let colors = theme::palette(cx);
        behavior_controls::action(id, name, colors)
            .aria_expanded(open)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .h_full()
            .w(ui_text::space(STRIP_BUTTON))
            .text_color(rgb(colors.muted))
            .when(open, |button| button.bg(rgb(colors.panel_active)))
            .hover(move |style| {
                style
                    .bg(rgb(colors.panel_active))
                    .text_color(rgb(colors.text))
            })
            .child(icon)
            .child(tooltip::anchor(name, Look::Pane))
    }

    /// A built-in panel of the pane: a symbol in the leading segment, pressed while shown.
    fn strip_panel(&self, pane_id: PaneId, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let pane = &self.panes[&pane_id];
        let tab = &pane.tabs[index];
        let kind = tab.panel().expect("a panel tab");
        let tab_id = tab.id;
        let active = index == pane.active;
        let label = Self::panel_label(kind);
        let ink = match active {
            true if !colors.plain_tabs => colors.magenta,
            true => colors.text,
            false => colors.muted,
        };
        let pane_selected = self.active_pane == pane_id;
        let workspace = cx.entity();
        behavior_controls::toggle_content(
            ("tab", tab_id),
            label,
            icons::text_icon(Icon::Panel(kind), 10.0, ink),
            active,
        )
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h_full()
        .w(ui_text::space(NATIVE_ICON_TAB_FULL))
        .border_1()
        .border_color(gpui::transparent_black())
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .map(|cell| flat_cell(cell, active, pane_selected, colors))
        .text_color(rgb(ink))
        .when(!active || !colors.plain_tabs, |cell| {
            cell.hover(move |style| {
                style
                    .bg(rgb(colors.panel_active))
                    .text_color(rgb(colors.text))
            })
        })
        .drag_over::<DraggedTab>(move |style, _, _, _| {
            style.border_l_2().border_color(rgb(colors.cyan))
        })
        .child(tooltip::anchor(panel_tooltip(kind), Look::Pane))
        .on_change({
            let listener = cx.listener(move |workspace, _: &gpui::ClickEvent, window, cx| {
                workspace.select_tab(pane_id, tab_id, window, cx);
            });
            move |_, event, window, cx| listener(event, window, cx)
        })
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |workspace, _, window, cx| {
                workspace.close_tab_by_user(pane_id, tab_id, window, cx);
            }),
        )
        .on_drag(
            DraggedTab {
                pane_id,
                tab_id,
                project_id: self.project_id.clone(),
                title: label.to_owned(),
            },
            move |drag, _, _, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.panel_menu = None;
                    workspace.strip_menu = None;
                    workspace.begin_tab_drag(cx);
                });
                cx.new(|_| drag.clone())
            },
        )
        .on_drop(
            cx.listener(move |workspace, drag: &DraggedTab, window, cx| {
                workspace.strip_drop(drag, pane_id, index, Some(tab_id), window, cx);
                cx.stop_propagation();
            }),
        )
        .into_any_element()
    }

    /// A session tab: status dot, canonical title, and a close mark shown on the selected
    /// tab and under the pointer. Right click, Shift+F10 or the Menu key opens its actions;
    /// a middle click closes it.
    #[allow(clippy::too_many_arguments)]
    fn strip_tab(
        &self,
        pane_id: PaneId,
        index: usize,
        slot: StripSlot,
        pane_selected: bool,
        tab_can_close: bool,
        error: u32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let native = ui_text::is_native();
        let pane = &self.panes[&pane_id];
        let tab = &pane.tabs[index];
        let tab_id = tab.id;
        let active = index == pane.active;
        let key = session_tab_key(tab);
        let pinned = slot == StripSlot::Pinned;
        let shared = key
            .as_deref()
            .is_some_and(|key| self.shared_tab_entries().iter().any(|e| e.key == key));
        let title = self.strip_title(tab, cx);
        let status = self.strip_status(tab, cx);
        let closable = tab_can_close && !pinned;
        let spoken = {
            let mut name = title.clone();
            if pinned {
                name.push_str(", pinned");
            }
            if let Some(status) = &status {
                name.push_str(", ");
                name.push_str(status_word(status));
            }
            name
        };
        let hint = match tab.chat() {
            Some(view) => chat_tab_hint(&view.read(cx).summary()),
            None => tab
                .shell_id()
                .and_then(|id| self.agent_activity.get(id))
                .and_then(AgentState::hint),
        };
        let ink = if active { colors.text } else { colors.muted };
        let workspace = cx.entity();
        let menu_key = key.clone().filter(|_| shared);
        let keyboard_key = menu_key.clone();
        let element = behavior_controls::action(("tab", tab_id), spoken, colors)
            .role(gpui::Role::Tab)
            .aria_selected(active)
            .group(TAB_GROUP)
            .flex()
            .flex_none()
            .items_center()
            .h_full()
            .gap(ui_text::space(6.0))
            .pl(ui_text::space(8.0))
            .pr(ui_text::space(if closable { 4.0 } else { 8.0 }))
            .text_size(ui_text::text(10.0))
            .text_color(rgb(ink))
            .map(|tab| flat_cell(tab, active, pane_selected, colors))
            .when(!(active && colors.plain_tabs), |tab| {
                tab.hover(move |style| {
                    style
                        .bg(rgb(colors.panel_active))
                        .text_color(rgb(colors.text))
                })
            })
            .drag_over::<DraggedTab>(move |style, _, _, _| {
                style.border_l_2().border_color(rgb(colors.cyan))
            })
            .children(pinned.then(|| {
                if native {
                    icons::symbol("pin.fill", 8.0, Some(ink))
                } else {
                    div()
                        .text_color(rgb(colors.magenta))
                        .child("◆")
                        .into_any_element()
                }
            }))
            .children(
                status
                    .as_ref()
                    .map(|status| status_dot(status, colors, error)),
            )
            .child(
                div()
                    .min_w_0()
                    .max_w(ui_text::space(TITLE_MAX_WIDTH))
                    .truncate()
                    .child(title.clone())
                    .children(hint.map(|hint| tooltip::anchor(hint, Look::Pane))),
            )
            .children(closable.then(|| {
                behavior_controls::action(("close-tab", tab_id), format!("Close {title}"), colors)
                    .size(ui_text::space(16.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .text_color(rgb(colors.muted))
                    .hover(move |style| style.bg(rgb(colors.divider)).text_color(rgb(colors.text)))
                    .when(!active, |close| {
                        behavior_controls::tab_close_reveal(close, TAB_GROUP)
                    })
                    .child(icons::text_icon(Icon::Close, 9.0, colors.muted))
                    .child(tooltip::anchor("Close tab · ⌘W", Look::Pane))
                    .on_click(cx.listener(move |workspace, _, window, cx| {
                        cx.stop_propagation();
                        workspace.close_tab_by_user(pane_id, tab_id, window, cx);
                    }))
                    .map(|close| {
                        behavior_controls::tab_close_boundary(("close-tab-boundary", tab_id), close)
                    })
            }))
            .on_click(cx.listener(move |workspace, _, window, cx| {
                workspace.select_tab(pane_id, tab_id, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |workspace, _, window, cx| {
                    cx.stop_propagation();
                    workspace.close_tab_by_user(pane_id, tab_id, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |workspace, _, window, cx| {
                    if let Some(key) = &menu_key {
                        // Keep the press from focusing the tab: the menu holds focus.
                        window.prevent_default();
                        workspace.open_shared_tab_menu(key, window, cx);
                    }
                }),
            )
            .on_key_down(
                cx.listener(move |workspace, event: &KeyDownEvent, window, cx| {
                    let menu = event.keystroke.key == "menu"
                        || (event.keystroke.key == "f10" && event.keystroke.modifiers.shift);
                    if menu && let Some(key) = &keyboard_key {
                        workspace.open_shared_tab_menu(key, window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .on_drag(
                DraggedTab {
                    pane_id,
                    tab_id,
                    project_id: self.project_id.clone(),
                    title: title.clone(),
                },
                move |drag, _, _, cx| {
                    workspace.update(cx, |workspace, cx| {
                        workspace.panel_menu = None;
                        workspace.strip_menu = None;
                        workspace.begin_tab_drag(cx);
                    });
                    cx.new(|_| drag.clone())
                },
            )
            .on_drop(
                cx.listener(move |workspace, drag: &DraggedTab, window, cx| {
                    workspace.strip_drop(drag, pane_id, index, Some(tab_id), window, cx);
                    cx.stop_propagation();
                }),
            );
        // Every tab hosts its menu's popup, so the host has measured the tab before the menu
        // opens and the menu (and its focus) is there on its first frame.
        let menu = self
            .shared_tab_menu
            .clone()
            .filter(|_| key.as_deref() == Some(self.shared_tab_menu_key.as_str()));
        behavior_controls::popup(("tab-menu-host", tab_id), element)
            .anchor(gpui::Anchor::TopLeft)
            .when_some(menu, |popup, menu| popup.content(menu))
            .into_any_element()
    }

    /// The open strip menu of `pane_id`, hung from its ＋ or All tabs button.
    fn render_strip_menu(&self, pane_id: PaneId, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let Some(menu) = self.strip_menu.filter(|menu| menu.pane == pane_id) else {
            return div().into_any_element();
        };
        let error = theme::diff_colors(cx).removed;
        let (name, rows): (&str, Vec<AnyElement>) = match menu.kind {
            StripMenuKind::New => (
                "New tab",
                [
                    ("New terminal", "⌘T", Some(Icon::Add), 0usize),
                    ("New Claude chat", "⌘⌥⇧L", None, 1),
                    ("New Codex chat", "⌘⌥⇧C", None, 2),
                ]
                .into_iter()
                .map(|(label, shortcut, icon, action)| {
                    self.strip_menu_row(
                        format!("strip-menu-new-{action}"),
                        label.to_owned(),
                        None,
                        icon.map(|icon| menu_icon(icon, colors)),
                        shortcut,
                        false,
                        move |workspace, window, cx| {
                            workspace.close_strip_menu(window, cx);
                            workspace.active_pane = pane_id;
                            match action {
                                0 => workspace.add_tab(window, cx),
                                1 => workspace.add_chat(Provider::Claude, false, window, cx),
                                _ => workspace.add_chat(Provider::Codex, false, window, cx),
                            }
                        },
                        cx,
                    )
                })
                .collect::<Vec<_>>()
                .into_iter()
                .chain([
                    controls::menu_separator(colors),
                    self.strip_menu_row(
                        "strip-menu-workers".to_owned(),
                        "Open a worker or shell…".to_owned(),
                        None,
                        None,
                        "",
                        false,
                        move |workspace, window, cx| {
                            workspace.strip_menu = Some(StripMenu {
                                pane: pane_id,
                                kind: StripMenuKind::Workers,
                            });
                            workspace.menu_focus.focus(window, cx);
                            cx.notify();
                        },
                        cx,
                    ),
                ])
                .collect(),
            ),
            StripMenuKind::Workers => {
                let entries = self.shared_tab_entries();
                let hidden = entries
                    .iter()
                    .filter(|e| {
                        e.hidden
                            && !(e.kind == project_tabs::Kind::Shell
                                && e.status == project_tabs::Status::Stopped)
                    })
                    .collect::<Vec<_>>();
                let mut rows = vec![pane_menu_heading("Open a worker or shell", true, colors)];
                if hidden.is_empty() {
                    rows.push(
                        div()
                            .px(ui_text::space(8.0))
                            .py(ui_text::space(5.0))
                            .text_color(rgb(colors.muted))
                            .child("No hidden workers or shells")
                            .into_any_element(),
                    );
                }
                rows.extend(hidden.into_iter().map(|entry| {
                    let key = entry.key.clone();
                    let kind = match entry.kind {
                        project_tabs::Kind::Chat => "Chat",
                        project_tabs::Kind::Shell => "Shell",
                    };
                    let parent = entry.parent.as_deref().and_then(|parent| {
                        entries
                            .iter()
                            .find(|e| e.key == parent)
                            .map(|e| e.title.clone())
                    });
                    let mut detail = if entry.worker {
                        format!(
                            "Worker {} · {}",
                            kind.to_lowercase(),
                            status_word(&entry.status)
                        )
                    } else {
                        format!("{kind} · {}", status_word(&entry.status))
                    };
                    if let Some(parent) = parent {
                        detail.push_str(&format!(" · from {parent}"));
                    }
                    self.strip_menu_row(
                        format!("open-worker-{key}"),
                        entry.title.clone(),
                        Some(detail),
                        Some(status_dot(&entry.status, colors, error)),
                        "",
                        false,
                        move |workspace, window, cx| {
                            workspace.close_strip_menu(window, cx);
                            workspace.active_pane = pane_id;
                            if let Err(error) = workspace.open_child_tab(&key, window, cx) {
                                workspace.notice = Some(error);
                            }
                            cx.notify();
                        },
                        cx,
                    )
                }));
                ("Open a worker or shell", rows)
            }
            StripMenuKind::AllTabs => {
                let pane = &self.panes[&pane_id];
                let slots = self.strip_slots(pane);
                let rows = strip_order(&slots)
                    .into_iter()
                    .filter(|index| slots[*index].0 != StripSlot::Panel)
                    .map(|index| {
                        let tab = &pane.tabs[index];
                        let tab_id = tab.id;
                        let status = self.strip_status(tab, cx);
                        self.strip_menu_row(
                            format!("strip-all-{tab_id}"),
                            self.strip_title(tab, cx),
                            None,
                            status.map(|status| status_dot(&status, colors, error)),
                            "",
                            index == pane.active,
                            move |workspace, window, cx| {
                                workspace.close_strip_menu(window, cx);
                                workspace.select_tab(pane_id, tab_id, window, cx);
                            },
                            cx,
                        )
                    })
                    .collect();
                ("All tabs", rows)
            }
        };
        behavior_controls::focus_scope(
            div()
                .id(("strip-menu", pane_id))
                .role(gpui::Role::Menu)
                .aria_label(name)
                .w(ui_text::space(260.0))
                .max_h(ui_text::space(420.0))
                .overflow_y_scroll()
                .bg(rgb(colors.panel_active))
                .border_1()
                .border_color(rgb(colors.magenta))
                .p(ui_text::space(3.0))
                .text_size(ui_text::text(10.0))
                .text_color(rgb(colors.text))
                .font_family(ui_text::ui_family())
                .map(|menu| controls::native(menu, |menu| controls::menu(menu, colors)))
                .on_mouse_down_out(cx.listener(|workspace, _, window, cx| {
                    workspace.close_strip_menu(window, cx);
                }))
                .on_key_down(cx.listener(|workspace, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => {
                            workspace.close_strip_menu(window, cx);
                            cx.stop_propagation();
                        }
                        // The Kit Root's trapped traversal: focus wraps inside the menu, so
                        // Escape always reaches it.
                        "down" | "up" => {
                            crate::project_settings::modal_tab(
                                event.keystroke.key == "up",
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                }))
                .children(rows),
            ("strip-menu-scope", pane_id),
            &self.menu_focus,
        )
        .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn strip_menu_row(
        &self,
        id: String,
        label: String,
        detail: Option<String>,
        lead: Option<AnyElement>,
        shortcut: &'static str,
        checked: bool,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let name = match &detail {
            Some(detail) => format!("{label}, {detail}"),
            None => label.clone(),
        };
        behavior_controls::action(gpui::SharedString::from(id), name, colors)
            .role(if checked {
                gpui::Role::MenuItemRadio
            } else {
                gpui::Role::MenuItem
            })
            .when(checked, |row| row.aria_toggled(gpui::Toggled::True))
            .flex()
            .items_center()
            .gap(ui_text::space(8.0))
            .px(ui_text::space(8.0))
            .py(ui_text::space(5.0))
            .text_size(ui_text::text(10.0))
            .text_color(rgb(colors.text))
            .hover(move |style| {
                controls::hovered(style, controls::menu_row_hover(colors), |style| {
                    style.bg(rgb(colors.divider)).text_color(rgb(colors.cyan))
                })
            })
            .map(|row| controls::native(row, |row| controls::menu_row(row, colors)))
            .child(
                div()
                    .w(ui_text::space(14.0))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .children(if checked {
                        Some(menu_icon(Icon::Check, colors))
                    } else {
                        lead
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_ellipsis().child(label))
                    .children(detail.map(|detail| {
                        div()
                            .text_ellipsis()
                            .text_size(ui_text::text(9.0))
                            .text_color(rgb(colors.muted))
                            .child(detail)
                    })),
            )
            .children((!shortcut.is_empty()).then(|| {
                div()
                    .flex_none()
                    .text_size(ui_text::text(9.0))
                    .text_color(rgb(colors.muted))
                    .child(shortcut)
            }))
            .on_click(cx.listener(move |workspace, _, window, cx| action(workspace, window, cx)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use StripSlot::{Panel, Pinned, Session};

    #[test]
    fn panels_lead_then_pinned_sessions_then_the_rest_in_shared_order() {
        let slots = [
            (Session, 7),
            (Panel, 0),
            (Pinned, 1),
            (Session, 3),
            (Panel, 0),
            (Pinned, 0),
        ];
        assert_eq!(strip_order(&slots), vec![1, 4, 5, 2, 3, 0]);
        // A view without a shared entry keeps its place after the shared tabs.
        let slots = [(Session, usize::MAX), (Session, 2), (Session, usize::MAX)];
        assert_eq!(strip_order(&slots), vec![1, 0, 2]);
    }

    #[test]
    fn command_numbers_count_session_tabs_and_nine_is_the_last() {
        let slots = [
            (Panel, 0),
            (Session, 2),
            (Pinned, 0),
            (Session, 3),
            (Session, 4),
        ];
        let order = strip_order(&slots);
        assert_eq!(numbered_session(&order, &slots, 1), Some(2));
        assert_eq!(numbered_session(&order, &slots, 2), Some(1));
        assert_eq!(numbered_session(&order, &slots, 4), Some(4));
        assert_eq!(numbered_session(&order, &slots, 5), None);
        assert_eq!(numbered_session(&order, &slots, 9), Some(4));
        assert_eq!(numbered_session(&[0], &[(Panel, 0)], 9), None);
        assert_eq!(numbered_session(&order, &slots, 0), None);
    }

    fn entry(key: &str, pinned: bool, parent: Option<&str>) -> project_tabs::Entry {
        serde_json::from_value(serde_json::json!({"key":key,"kind":"chat","title":key,"status":"done","pinned":pinned,"hidden":false,"worker":false,"order":0,"parent":parent,"children":[],"child_count":0})).unwrap()
    }

    #[test]
    fn a_strip_drag_moves_within_its_pin_group_across_parents() {
        let entries = [
            entry("p", true, None),
            entry("a", false, None),
            entry("b", false, None),
            entry("c", false, None),
            entry("w", false, Some("a")),
        ];
        let drawn = ["p", "a", "b", "c", "w"]
            .iter()
            .enumerate()
            .map(|(i, key)| (i as TabId, key.to_string()))
            .collect::<Vec<_>>();
        // Leftward lands before the target, rightward after it.
        assert_eq!(strip_move(&entries, &drawn, 3, 1), Some(Some("a".into())));
        assert_eq!(strip_move(&entries, &drawn, 1, 2), Some(Some("c".into())));
        assert_eq!(strip_move(&entries, &drawn, 1, 3), Some(Some("w".into())));
        // A root dropped on another tab's worker lands beside it, and a worker among roots:
        // the drop the strip used to refuse, so the dragged tab sprang back.
        assert_eq!(strip_move(&entries, &drawn, 2, 4), Some(None));
        assert_eq!(strip_move(&entries, &drawn, 4, 2), Some(Some("b".into())));
        // No move onto itself or into the pinned group.
        assert_eq!(strip_move(&entries, &drawn, 2, 2), None);
        assert_eq!(strip_move(&entries, &drawn, 2, 0), None);
        assert_eq!(strip_move(&entries, &drawn, 0, 2), None);
    }

    #[test]
    fn pinned_tabs_scroll_when_they_would_take_over_half_the_strip() {
        assert!(!pins_scroll(200.0, 400.0));
        assert!(pins_scroll(201.0, 400.0));
        assert!(pins_scroll(10.0, 0.0));
    }

    #[test]
    fn all_tabs_appears_only_when_the_scrolling_tabs_do_not_fit() {
        assert!(!strip_overflows(&[], 3.0, 0.0));
        assert!(!strip_overflows(&[100.0, 100.0], 3.0, 203.0));
        assert!(strip_overflows(&[100.0, 100.0], 3.0, 202.0));
    }

    #[test]
    fn stopped_and_unknown_activity_map_to_the_phones_states() {
        assert_eq!(
            activity_status(AgentActivity::Working),
            Some(project_tabs::Status::Working)
        );
        assert_eq!(
            activity_status(AgentActivity::Waiting),
            Some(project_tabs::Status::Waiting)
        );
        assert_eq!(
            activity_status(AgentActivity::Exited),
            Some(project_tabs::Status::Stopped)
        );
        assert_eq!(activity_status(AgentActivity::Unknown), None);
        assert_eq!(status_word(&project_tabs::Status::Error), "error");
    }
}
