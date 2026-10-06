//! Compact scheduling tab, using RiWork's existing GPUI input and palette.
use crate::text_input::{self, EnterBehavior, InputEvent, InputState};
use crate::{
    behavior_controls as behavior,
    chat::model::{ApprovalMode, ChatInfo, Provider},
    controls,
    layouts::PanelKind,
    project_settings::Input,
    schedules::{self, Schedule, ScheduleStore, Scope, Target, Timing},
    sessions::{HarnessKind, SessionManager, ShellSession},
    store::{State, Store},
    theme, ui_text,
};
use chrono::{DateTime, Local};
use gpui::{
    AnyElement, Context, FocusHandle, IntoElement, KeyDownEvent, Render, Window, div, prelude::*,
    rgb,
};
use gpui::{Entity, Focusable, Subscription};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Control {
    Field(usize),
    Destination(bool),
    Provider(Provider),
    Permission(ApprovalMode),
    Fast,
    OpenChat(String),
    Scope(usize),
    Target(ExistingTarget),
    Workspace(String),
    Repeat(u64),
    FirstIn(u64),
    ExactTime,
    Save,
    Cancel,
    New,
    Edit(String),
    Pause(String),
    Delete(String),
}
fn control_id(control: &Control) -> String {
    format!("schedule-control-{control:?}")
}

fn paint_action<E: gpui::Styled + gpui::InteractiveElement>(
    element: E,
    selected: bool,
    focused: bool,
    colors: crate::theme::Palette,
) -> E {
    let element = element
        .min_w_0()
        .overflow_hidden()
        .text_ellipsis()
        .focus_visible(move |style| style.border_color(rgb(colors.focus)));
    if ui_text::is_native() {
        let kind = if selected {
            controls::Button::Primary
        } else {
            controls::Button::Secondary
        };
        return controls::button(element, kind, colors)
            .py(ui_text::space(3.0))
            .whitespace_nowrap()
            .hover(move |style| style.bg(rgb(kind.hover(colors))))
            .when(focused, |button| button.border_color(rgb(colors.focus)));
    }
    element
        .px(ui_text::space(8.0))
        .py(ui_text::space(5.0))
        .border_1()
        .border_color(rgb(if focused {
            colors.focus
        } else if selected {
            colors.cyan
        } else {
            colors.divider
        }))
        .bg(rgb(if selected {
            colors.panel_active
        } else {
            colors.bg
        }))
        .text_color(rgb(if selected { colors.cyan } else { colors.text }))
}

/// Presentation/interaction factory shared by the panel and inert event fixtures.
/// Rebinding commands intentionally remain buttons: reselecting a target is meaningful.
#[allow(clippy::too_many_arguments)]
fn action_control(
    label: String,
    action: &Control,
    selected: bool,
    pressed: bool,
    disabled: bool,
    focus: &FocusHandle,
    focused: bool,
    colors: crate::theme::Palette,
    activate: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let id = control_id(action);
    let ax_id = id.clone();
    if matches!(action, Control::Permission(_) | Control::Repeat(_)) {
        return paint_action(
            behavior::radio_content(id, label.clone(), label, selected)
                .disabled(disabled)
                .track_focus(focus),
            selected,
            focused,
            colors,
        )
        .on_change(move |_, event, window, cx| activate(event, window, cx))
        .map(|control| crate::form_input::control_element(ax_id, control))
        .into_any_element();
    }
    if matches!(
        action,
        Control::Fast | Control::ExactTime | Control::Pause(_)
    ) {
        return paint_action(
            behavior::toggle_content(id, label.clone(), label, pressed)
                .disabled(disabled)
                .track_focus(focus),
            selected,
            focused,
            colors,
        )
        .on_change(move |_, event, window, cx| activate(event, window, cx))
        .map(|control| crate::form_input::control_element(ax_id, control))
        .into_any_element();
    }
    paint_action(
        behavior::button_content(id, label.clone(), label)
            .disabled(disabled)
            .track_focus(focus)
            .aria_selected(selected),
        selected,
        focused,
        colors,
    )
    .on_click(activate)
    .map(|control| crate::form_input::control_element(ax_id, control))
    .into_any_element()
}

/// Retains the displayed destination type through the asynchronous save.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ExistingTarget {
    Shell(String),
    LegacyChat(String),
}
impl ExistingTarget {
    fn id(&self) -> &str {
        match self {
            Self::Shell(id) | Self::LegacyChat(id) => id,
        }
    }
    fn bind(
        &self,
        scope: Scope,
        state: &State,
        sessions: &SessionManager,
        schedules: &ScheduleStore,
    ) -> Result<Target, String> {
        match self {
            Self::Shell(id) => Target::bind_shell(scope, state, sessions, id),
            Self::LegacyChat(id) => {
                let chats = schedules.chat_orchestrators();
                let chat = chats.iter().find(|chat| &chat.id == id)
                    .ok_or("Selected legacy chat is unavailable; reopen the editor and select an existing target")?;
                Target::bind_chat(scope, state, chat)
            }
        }
    }
}
struct Editor {
    previous: Option<Schedule>,
    scope: usize,
    selection: Option<ExistingTarget>,
    pinned: Option<Target>,
    repeat: u64,
    first_quick: Option<u64>,
    exact_time: bool,
    fields: [Input; 6],
    fresh: bool,
    provider: Provider,
    permission: ApprovalMode,
    fast: bool,
}
impl Editor {
    fn select_target(&mut self, selection: ExistingTarget) {
        self.selection = Some(selection);
        self.pinned = None;
    }
    fn select_scope(&mut self, scope: usize) {
        self.scope = scope;
        self.selection = None;
        self.pinned = None;
    }
    fn pinned_target(&self) -> Result<Option<Target>, String> {
        let mut target = self.pinned.clone();
        if self.fresh
            && let Some(target) = &mut target
        {
            target.set_new_chat_options(
                optional_option(&self.fields[4].text),
                optional_option(&self.fields[5].text),
                self.fast,
                self.permission,
            )?;
        }
        Ok(target)
    }
    fn new() -> Self {
        Self {
            previous: None,
            fresh: true,
            provider: Provider::Codex,
            permission: ApprovalMode::Supervised,
            fast: false,
            scope: 1,
            selection: None,
            pinned: None,
            repeat: 0,
            first_quick: Some(600),
            exact_time: false,
            fields: [
                Input::default(),
                Input::default(),
                Input::new(format_time(schedules::now() + 600)),
                Input::new("60".into()),
                Input::default(),
                Input::default(),
            ],
        }
    }

    fn first_in(&mut self, seconds: u64, now: u64) {
        self.fields[2] = Input::new(format_time(now + seconds));
        self.first_quick = Some(seconds);
    }
}
pub enum SchedulePanelEvent {
    OpenChat(String),
}
impl gpui::EventEmitter<SchedulePanelEvent> for SchedulePanel {}
pub struct SchedulePanel {
    store: Store,
    sessions: SessionManager,
    schedules: ScheduleStore,
    project_id: String,
    workspace_id: Option<String>,
    state: State,
    targets: Vec<ShellSession>,
    /// The orchestrators that run as chats: targets of the app and project scopes.
    chats: Vec<ChatInfo>,
    rows: Vec<Schedule>,
    project_chats: Vec<ChatInfo>,
    editor: Option<Editor>,
    controls: Vec<Control>,
    control_focus: std::collections::BTreeMap<String, FocusHandle>,
    active: usize,
    focus: FocusHandle,
    error: Option<String>,
    pending: bool,
    refreshing: bool,
    delete_confirm: Option<String>,
    input_states: Vec<Entity<InputState>>,
    input_subscriptions: Vec<Subscription>,
}
impl SchedulePanel {
    pub fn new(
        store: Store,
        sessions: SessionManager,
        project_id: String,
        workspace_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let schedules =
            ScheduleStore::at(sessions.state_home().to_path_buf()).expect("schedule store");
        cx.observe_global::<theme::Appearance>(|_, cx| cx.notify())
            .detach();
        cx.observe_global::<schedules::RuntimeStatus>(|_, cx| cx.notify())
            .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |panel, cx| panel.refresh(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let mut panel = Self {
            store,
            sessions,
            schedules,
            project_id,
            workspace_id,
            state: State::default(),
            targets: vec![],
            chats: vec![],
            rows: vec![],
            project_chats: vec![],
            editor: None,
            controls: vec![],
            control_focus: std::collections::BTreeMap::new(),
            active: 0,
            focus: cx.focus_handle(),
            error: None,
            pending: false,
            refreshing: false,
            delete_confirm: None,
            input_states: Vec::new(),
            input_subscriptions: Vec::new(),
        };
        panel.refresh(cx);
        panel
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_control(window, cx);
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refreshing || self.pending {
            return;
        }
        self.refreshing = true;
        let store = self.store.clone();
        let sessions = self.sessions.clone();
        let schedules = self.schedules.clone();
        let work = cx.background_executor().spawn(async move {
            // The shells come from the sample every window's refresh shares (at
            // most a tick old), not from tmux again: this runs every second.
            Ok::<_, String>((
                store.snapshot()?,
                sessions.sample(false)?.shells,
                schedules.list()?,
                schedules.chat_orchestrators(),
                crate::chat::log::read_infos(sessions.state_home()),
            ))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |panel, cx| {
                panel.refreshing = false;
                // Nearly every second finds everything as it was.
                let changed = match result {
                    Ok((state, targets, rows, chats, all_chats)) => {
                        let rows: Vec<_> = rows
                            .into_iter()
                            .filter(|s| s.target.scope.visible(&panel.project_id))
                            .collect();
                        let project_chats: Vec<_> = all_chats
                            .into_iter()
                            .filter(|chat| {
                                chat.project_id.as_ref() == Some(&panel.project_id)
                                    && chat.orchestrator.is_none()
                            })
                            .collect();
                        let changed = panel.project_chats != project_chats
                            || panel.state != state
                            || panel.targets != targets
                            || panel.chats != chats
                            || panel.rows != rows;
                        panel.project_chats = project_chats;
                        panel.state = state;
                        panel.targets = targets;
                        panel.chats = chats;
                        panel.rows = rows;
                        changed
                    }
                    Err(e) => {
                        let changed = panel.error.as_ref() != Some(&e);
                        panel.error = Some(e);
                        changed
                    }
                };
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }
    fn scope(&self, index: usize) -> Result<Scope, String> {
        Ok(match index {
            0 => Scope::App,
            1 => Scope::Project {
                project_id: self.project_id.clone(),
            },
            _ => Scope::Workspace {
                project_id: self.project_id.clone(),
                worktree_id: self
                    .workspace_id
                    .clone()
                    .ok_or("Select a worktree in WORKTREES before creating a workspace schedule")?,
            },
        })
    }
    fn open_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input_subscriptions.clear();
        self.input_states = self
            .editor
            .as_ref()
            .unwrap()
            .fields
            .iter()
            .map(|draft| text_input::single_line(draft.text.clone(), "", window, cx))
            .collect();
        for (index, state) in self.input_states.iter().enumerate() {
            self.input_subscriptions.push(cx.subscribe_in(
                state,
                window,
                move |panel, state, event, _, cx| {
                    if panel.pending {
                        return;
                    }
                    match event {
                        InputEvent::Change => {
                            if let Some(editor) = &mut panel.editor {
                                editor.fields[index].text = state.read(cx).value().to_string();
                                if index == 2 {
                                    editor.first_quick = None;
                                }
                            }
                            panel.error = None;
                            cx.notify();
                        }
                        InputEvent::Focus => {
                            if let Some(at) = panel
                                .controls
                                .iter()
                                .position(|control| *control == Control::Field(index))
                            {
                                panel.active = at;
                            }
                            cx.notify();
                        }
                        _ if text_input::is_submit(event, EnterBehavior::Submit)
                            && panel.editor.is_some() =>
                        {
                            panel.save(cx)
                        }
                        _ => {}
                    }
                },
            ));
        }
    }
    fn focus_control(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Control::Field(index)) = self.controls.get(self.active)
            && let Some(input) = self.input_states.get(*index)
        {
            input.read(cx).focus_handle(cx).focus(window, cx);
        } else if let Some(focus) = self
            .controls
            .get(self.active)
            .and_then(|control| self.control_focus.get(&control_id(control)))
        {
            focus.focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
    }
    fn accepts_input(&self) -> bool {
        !self.pending
            && self.editor.is_some()
            && matches!(self.controls.get(self.active), Some(Control::Field(_)))
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.controls.iter().position(|control| {
            self.control_focus
                .get(&control_id(control))
                .is_some_and(|focus| focus.is_focused(window))
        }) {
            self.active = index;
        }

        if matches!(event.keystroke.key.as_str(), "escape" | "tab")
            && let Some(Control::Field(index)) = self.controls.get(self.active)
            && let Some(state) = self.input_states.get(*index)
            && crate::form_input::is_composing(state, window, cx)
        {
            return;
        }
        if self.pending {
            return;
        }
        match event.keystroke.key.as_str() {
            "tab" => {
                let count = self.controls.len();
                if count > 0 {
                    self.active = (self.active
                        + if event.keystroke.modifiers.shift {
                            count - 1
                        } else {
                            1
                        })
                        % count;
                }
            }
            "escape" => {
                self.editor = None;
                self.input_states.clear();
                self.input_subscriptions.clear();
                self.active = 0;
                self.delete_confirm = None;
            }
            "s" if event.keystroke.modifiers.platform
                && !event.keystroke.modifiers.shift
                && self.editor.is_some() =>
            {
                self.save(cx)
            }
            _ => return,
        }
        self.focus_control(window, cx);
        cx.stop_propagation();
        cx.notify();
    }
    fn perform(&mut self, control: Control, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let opening = matches!(&control, Control::New | Control::Edit(_));
        let preset = matches!(&control, Control::FirstIn(_));
        self.error = None;
        let deleting = matches!(control, Control::Delete(_));
        match control {
            Control::Field(_) => {}
            Control::OpenChat(id) => cx.emit(SchedulePanelEvent::OpenChat(id)),
            Control::Destination(fresh) => {
                if let Some(e) = &mut self.editor {
                    e.fresh = fresh;
                    e.pinned = None;
                    e.selection = None;
                    e.scope = 1;
                }
            }
            Control::Provider(provider) => {
                if let Some(e) = &mut self.editor {
                    e.provider = provider;
                    e.pinned = None;
                }
            }
            Control::Permission(permission) => {
                if let Some(e) = &mut self.editor {
                    e.permission = permission;
                }
            }
            Control::Fast => {
                if let Some(e) = &mut self.editor {
                    e.fast = !e.fast;
                }
            }
            Control::New => {
                self.editor = Some(Editor::new());
                self.delete_confirm = None;
            }
            Control::Cancel => {
                self.editor = None;
                self.input_states.clear();
                self.input_subscriptions.clear();
                self.active = 0;
                self.delete_confirm = None;
            }
            Control::Scope(scope) => {
                if let Some(e) = &mut self.editor {
                    e.select_scope(scope);
                }
            }
            Control::Target(selection) => {
                if let Some(e) = &mut self.editor {
                    e.select_target(selection);
                }
            }
            Control::Workspace(id) => {
                self.workspace_id = Some(id);
                if let Some(e) = &mut self.editor {
                    e.selection = None;
                    e.pinned = None;
                }
            }
            Control::Repeat(seconds) => {
                if let Some(e) = &mut self.editor {
                    e.repeat = seconds;
                }
            }
            Control::FirstIn(seconds) => {
                if let Some(e) = &mut self.editor {
                    e.first_in(seconds, schedules::now());
                }
            }
            Control::ExactTime => {
                if let Some(e) = &mut self.editor {
                    e.exact_time = !e.exact_time;
                }
            }
            Control::Save => self.save(cx),
            Control::Edit(id) => {
                if let Some(s) = self.rows.iter().find(|s| s.id == id).cloned() {
                    let scope = match s.target.scope {
                        Scope::App => 0,
                        Scope::Project { .. } => 1,
                        Scope::Workspace {
                            ref worktree_id, ..
                        } => {
                            self.workspace_id = Some(worktree_id.clone());
                            2
                        }
                    };
                    let repeat = match s.timing {
                        Timing::Once { .. } => 0,
                        Timing::Interval { seconds, .. } => seconds,
                    };
                    let fresh = s.target.new_chat.as_ref();
                    self.editor = Some(Editor {
                        fresh: fresh.is_some(),
                        provider: fresh.map(|f| f.provider).unwrap_or(Provider::Codex),
                        permission: fresh.map(|f| f.approval_mode).unwrap_or_default(),
                        fast: fresh.is_some_and(|f| f.fast),
                        fields: [
                            Input::new(s.title.clone()),
                            Input::new(s.prompt.clone()),
                            Input::new(format_time(
                                s.next_run
                                    .filter(|n| *n > schedules::now())
                                    .unwrap_or(schedules::now() + 600),
                            )),
                            Input::new((repeat.max(300) / 60).to_string()),
                            Input::new(fresh.and_then(|f| f.model.clone()).unwrap_or_default()),
                            Input::new(fresh.and_then(|f| f.effort.clone()).unwrap_or_default()),
                        ],
                        scope,
                        selection: Some(if s.target.chat.is_some() {
                            ExistingTarget::LegacyChat(s.target.shell_id.clone())
                        } else {
                            ExistingTarget::Shell(s.target.shell_id.clone())
                        }),
                        pinned: Some(s.target.clone()),
                        first_quick: None,
                        exact_time: true,
                        repeat: if matches!(repeat, 0 | 3600 | 86400 | 604800) {
                            repeat
                        } else {
                            u64::MAX
                        },
                        previous: Some(s),
                    });
                    self.delete_confirm = None;
                }
            }
            Control::Pause(id) | Control::Delete(id) => {
                if let Some(row) = self.rows.iter().find(|s| s.id == id).cloned() {
                    if deleting && self.delete_confirm.as_ref() != Some(&id) {
                        self.delete_confirm = Some(id);
                        cx.notify();
                        return;
                    }
                    let store = self.schedules.clone();
                    self.pending = true;
                    let work = cx.background_executor().spawn(async move {
                        if deleting {
                            store.delete(&row.id, row.revision)
                        } else {
                            store
                                .pause(&row.id, row.revision, !row.paused, schedules::now())
                                .map(|_| ())
                        }
                    });
                    self.finish(work, cx);
                }
            }
        }
        if opening && self.editor.is_some() {
            self.open_inputs(window, cx);
        }
        if self.editor.is_none() {
            self.input_states.clear();
            self.input_subscriptions.clear();
        }
        if preset
            && let Some(editor) = &self.editor
            && let Some(state) = self.input_states.get(2)
        {
            let value = editor.fields[2].text.clone();
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        if opening && let Some(input) = self.input_states.first() {
            input.read(cx).focus_handle(cx).focus(window, cx);
        } else {
            self.focus_control(window, cx);
        }
        cx.notify();
    }
    fn finish(&mut self, work: gpui::Task<Result<(), String>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |panel, cx| {
                panel.pending = false;
                panel.delete_confirm = None;
                if let Err(e) = result {
                    panel.error = Some(e);
                }
                panel.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }
    fn save(&mut self, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        if let Some(editor) = &mut self.editor {
            for (draft, state) in editor.fields.iter_mut().zip(&self.input_states) {
                draft.text = state.read(cx).value().to_string();
            }
        }
        let result = (|| {
            let e = self.editor.as_ref().ok_or("No schedule editor")?;
            let parsed = DateTime::parse_from_rfc3339(e.fields[2].text.trim())
                .map_err(|_| "Use YYYY-MM-DDTHH:MM:SS+HH:MM with an explicit UTC offset")?;
            let at = u64::try_from(parsed.timestamp()).map_err(|_| "Date must be after 1970")?;
            let timing = selected_timing(e.repeat, at, &e.fields[3].text)?;
            let scope = self.scope(e.scope)?;
            let selection = if e.fresh {
                None
            } else {
                Some(
                    e.selection
                        .clone()
                        .ok_or("Choose an existing target session")?,
                )
            };
            Ok::<_, String>((
                e.previous.as_ref().map(|s| (s.id.clone(), s.revision)),
                e.fields[0].text.clone(),
                e.fields[1].text.clone(),
                scope,
                selection,
                e.pinned_target()?,
                timing,
                e.fresh,
                e.provider,
                e.permission,
                e.fast,
                (!e.fields[4].text.trim().is_empty()).then(|| e.fields[4].text.trim().to_owned()),
                (!e.fields[5].text.trim().is_empty()).then(|| e.fields[5].text.trim().to_owned()),
            ))
        })();
        let (
            previous,
            title,
            prompt,
            scope,
            selection,
            pinned,
            timing,
            fresh,
            provider,
            permission,
            fast,
            model,
            effort,
        ) = match result {
            Ok(v) => v,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        self.pending = true;
        let store = self.store.clone();
        let sessions = self.sessions.clone();
        let schedules = self.schedules.clone();
        let project_id = self.project_id.clone();
        let work = cx.background_executor().spawn(async move {
            let state = store.snapshot()?;
            let target = match pinned {
                Some(target) => target,
                None if fresh => Target::bind_new_chat(
                    sessions.state_home(),
                    &state,
                    &project_id,
                    provider,
                    model,
                    effort,
                    fast,
                    permission,
                    None,
                )?,
                None => selection
                    .ok_or("Choose an existing target session")?
                    .bind(scope, &state, &sessions, &schedules)?,
            };
            if target.new_chat.is_some() {
                target.validate_new_chat(sessions.state_home(), &state)?;
            }
            // Retain a pinned identity on edit; only an explicit target click rebinds it.
            schedules
                .save(
                    previous.as_ref().map(|(id, r)| (id.as_str(), *r)),
                    title,
                    prompt,
                    target,
                    timing,
                    schedules::now(),
                )
                .map(|_| ())
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |panel, cx| {
                panel.pending = false;
                match result {
                    Ok(()) => {
                        let inputs = panel.input_states.clone();
                        let focus = panel.focus.clone();
                        let owner = cx.entity_id();
                        cx.defer(move |app| {
                            app.with_window(owner, |window, app| {
                                if inputs.iter().any(|input| {
                                    input.read(app).focus_handle(app).is_focused(window)
                                }) {
                                    focus.focus(window, app);
                                }
                            });
                        });
                        panel.editor = None;
                        panel.input_states.clear();
                        panel.input_subscriptions.clear();
                        panel.error = None;
                    }
                    Err(e) => panel.error = Some(e),
                };
                panel.refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }
    fn button(
        &mut self,
        label: impl Into<String>,
        action: Control,
        selected: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = label.into();
        let colors = theme::palette(cx);
        let index = self.controls.len();
        let id = control_id(&action);
        let focus = self
            .control_focus
            .entry(id.clone())
            .or_insert_with(|| cx.focus_handle())
            .clone();
        self.controls.push(action.clone());
        let focused = focus.is_focused(window);
        let pressed = match &action {
            Control::Pause(id) => self
                .rows
                .iter()
                .find(|row| &row.id == id)
                .is_some_and(|row| row.paused),
            _ => selected,
        };
        let rendered_action = action.clone();
        let activate = cx.listener(move |panel, _, window, cx| {
            if panel.controls.get(index) != Some(&action) {
                return;
            }
            panel.active = index;
            panel.perform(action.clone(), window, cx);
        });
        action_control(
            label,
            &rendered_action,
            selected,
            pressed,
            self.pending,
            &focus,
            focused,
            colors,
            activate,
        )
    }
    fn field(
        &mut self,
        index: usize,
        label: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let control = self.controls.len();
        self.controls.push(Control::Field(index));
        let input = &self.input_states[index];
        div()
            .flex()
            .flex_col()
            .gap(ui_text::space(4.0))
            .min_w_0()
            .w_full()
            .child(
                div()
                    .text_size(ui_text::text(9.0))
                    .text_color(rgb(colors.muted))
                    .child(ui_text::cased(label.to_owned())),
            )
            .child(
                div()
                    .id(format!("schedule-field-{index}"))
                    .min_w_0()
                    .border_1()
                    .border_color(rgb(colors.divider))
                    // Native's field draws its own rounded edge.
                    .when(ui_text::is_native(), |field| field.border_0())
                    .child(crate::form_input::frame(
                        ("schedule-input", index),
                        input,
                        self.pending,
                        window,
                        cx,
                    ))
                    .on_click(cx.listener(move |panel, _, window, cx| {
                        panel.active = control;
                        panel.focus_control(window, cx);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}
impl Render for SchedulePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let focused_control = self
            .input_states
            .iter()
            .position(|input| input.read(cx).focus_handle(cx).is_focused(window))
            .map(Control::Field)
            .or_else(|| {
                self.controls
                    .iter()
                    .find(|control| {
                        self.control_focus
                            .get(&control_id(control))
                            .is_some_and(|focus| focus.is_focused(window))
                    })
                    .cloned()
            })
            .or_else(|| self.controls.get(self.active).cloned());
        self.controls.clear();
        let mut body = div()
            .w_full()
            .min_w_0()
            .max_w(ui_text::space(800.0))
            .flex()
            .flex_col()
            .gap(ui_text::space(12.0));
        let native = ui_text::is_native();
        // Native puts the title and the new-schedule button in the shared panel header.
        let header = native.then(|| {
            let index = self.controls.len();
            self.controls.push(Control::New);
            let focus = self
                .control_focus
                .entry(control_id(&Control::New))
                .or_insert_with(|| cx.focus_handle())
                .clone();
            let focused = focus.is_focused(window);
            let count = self.rows.len();
            controls::panel_header(
                PanelKind::Schedules.label(),
                Some(
                    match count {
                        0 => "No automations".to_owned(),
                        1 => "1 automation".to_owned(),
                        count => format!("{count} automations"),
                    }
                    .into(),
                ),
                [crate::project_settings::kit_toolbar_button(
                    "schedule-control-new",
                    "plus",
                    "New automation",
                    true,
                    colors,
                )
                .track_focus(&focus)
                .disabled(self.pending)
                .border_1()
                .border_color(if focused {
                    rgb(colors.focus).into()
                } else {
                    gpui::transparent_black()
                })
                .on_click(cx.listener(move |panel, _, window, cx| {
                    if panel.controls.get(index) != Some(&Control::New) {
                        return;
                    }
                    panel.active = index;
                    panel.perform(Control::New, window, cx);
                }))
                .into_any_element()],
                colors,
            )
        });
        if !native {
            body = body.child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_between()
                    .items_center()
                    .gap(ui_text::space(8.0))
                    .child(
                        div()
                            .text_color(rgb(colors.cyan))
                            .child(ui_text::cased("Automations")),
                    )
                    .child(self.button(
                        ui_text::cased("+ Schedule").to_string(),
                        Control::New,
                        false,
                        window,
                        cx,
                    )),
            );
        }
        body = body.child(
            div()
                .text_color(rgb(colors.muted))
                .text_size(ui_text::text(10.0))
                .when(native, |note| {
                    note.px(ui_text::space(
                        controls::PANEL_INSET - controls::LIST_MARGIN,
                    ))
                })
                .child("Schedule a new project chat or a prompt for an existing AI shell."),
        );
        if let Some(editor) = &self.editor {
            let scope_index = editor.scope;
            let fresh = editor.fresh;
            let provider = editor.provider;
            let permission = editor.permission;
            let fast = editor.fast;
            let repeat = editor.repeat;
            let selected = editor
                .selection
                .as_ref()
                .map(|target| target.id().to_owned());
            let editing = editor.previous.is_some();
            let first_quick = editor.first_quick;
            let exact_time = editor.exact_time;
            let first_display = DateTime::parse_from_rfc3339(&editor.fields[2].text)
                .map(|at| {
                    at.with_timezone(&Local)
                        .format("%Y-%m-%d %H:%M:%S %:z")
                        .to_string()
                })
                .unwrap_or_else(|_| "Choose a quick time or enter an exact date/time".into());
            let mut form = div()
                .flex()
                .flex_col()
                .gap(ui_text::space(10.0))
                .p(ui_text::space(12.0))
                .border_1()
                .border_color(rgb(colors.divider))
                .bg(rgb(colors.panel))
                .map(|form| controls::native(form, |form| controls::card(form, colors)));
            form = form.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(ui_text::space(5.0))
                    .child(self.button(
                        "New project chat".to_string(),
                        Control::Destination(true),
                        fresh,
                        window,
                        cx,
                    ))
                    .child(self.button(
                        "Existing AI shell".to_string(),
                        Control::Destination(false),
                        !fresh,
                        window,
                        cx,
                    )),
            );
            form = form
                .child(self.field(0, "Title", window, cx))
                .child(self.field(1, "Prompt · Single line", window, cx));
            if fresh {
                let mut choices = div().flex().flex_wrap().gap(ui_text::space(5.0));
                for (value, label) in [(Provider::Codex, "Codex"), (Provider::Claude, "Claude")] {
                    choices = choices.child(self.button(
                        label.to_string(),
                        Control::Provider(value),
                        value == provider,
                        window,
                        cx,
                    ));
                }
                form = form
                    .child(choices)
                    .child(self.field(4, "Model · Optional", window, cx))
                    .child(self.field(5, "Effort · Optional", window, cx));
                let mut permissions = gpui_kit::base::RadioGroup::new("automation-permission")
                    .aria_label("Permission mode")
                    .axis(gpui::Axis::Horizontal)
                    .flex()
                    .flex_wrap()
                    .gap(ui_text::space(5.0));
                for (value, label) in [
                    (ApprovalMode::Supervised, "Supervised"),
                    (ApprovalMode::AutoEdit, "Auto edit"),
                    (ApprovalMode::Full, "Full access"),
                    (ApprovalMode::Plan, "Plan"),
                ] {
                    permissions = permissions.child(self.button(
                        label.to_string(),
                        Control::Permission(value),
                        value == permission,
                        window,
                        cx,
                    ));
                }
                form = form.child(div().text_color(rgb(colors.muted)).child("Permission mode"))
                    .child(permissions).child(self.button("Fast mode".to_string(), Control::Fast, fast, window, cx))
                    .child(div().text_color(rgb(colors.muted)).child("Each run creates an ordinary chat at this project's root. The selected Codex account is pinned when saved."));
                if let Some(project) = self.state.projects.iter().find(|p| p.id == self.project_id)
                {
                    form = form.child(
                        div()
                            .text_size(ui_text::text(10.0))
                            .text_color(rgb(colors.muted))
                            .child(format!(
                                "Project: {} · {}",
                                project.name,
                                project.root.display()
                            )),
                    );
                }
                let account = self
                    .editor
                    .as_ref()
                    .and_then(|e| e.pinned.as_ref())
                    .and_then(|t| t.new_chat.as_ref())
                    .map(|f| {
                        f.codex_account_id
                            .clone()
                            .unwrap_or_else(|| "System default".into())
                    })
                    .unwrap_or_else(|| "Selected in Project Settings / Settings".into());
                form = form.child(
                    div()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .child(format!(
                            "Account: {}",
                            if provider == Provider::Claude {
                                "Claude system login"
                            } else {
                                &account
                            }
                        )),
                );
            }
            if !fresh {
                let mut scopes = div().flex().flex_wrap().gap(ui_text::space(5.0));
                for (i, label) in ["App", "Project", "Workspace"].iter().enumerate() {
                    scopes = scopes.child(self.button(
                        ui_text::cased(*label).to_string(),
                        Control::Scope(i),
                        i == scope_index,
                        window,
                        cx,
                    ));
                }
                form = form.child(scopes);
                if scope_index == 2 {
                    let workspaces: Vec<_> = self
                        .state
                        .worktrees_for(&self.project_id)
                        .into_iter()
                        .cloned()
                        .collect();
                    let mut choices = div().flex().flex_wrap().gap(ui_text::space(5.0));
                    for workspace in workspaces {
                        choices = choices.child(self.button(
                            format!("{} · {}", workspace.branch, workspace.id),
                            Control::Workspace(workspace.id.clone()),
                            self.workspace_id.as_ref() == Some(&workspace.id),
                            window,
                            cx,
                        ));
                    }
                    form = form
                        .child(
                            div()
                                .text_color(rgb(colors.muted))
                                .text_size(ui_text::text(9.0))
                                .child(ui_text::cased("Workspace / worktree")),
                        )
                        .child(choices);
                }
            }
            if !fresh {
                let scope = self.scope(scope_index);
                let targets: Vec<_> = self
                    .targets
                    .iter()
                    .filter(|s| {
                        s.alive
                            && s.harness.is_some_and(HarnessKind::schedulable)
                            && scope
                                .as_ref()
                                .is_ok_and(|scope| scope.matches_explicit_shell(&self.state, s))
                    })
                    .cloned()
                    .collect();
                let chats: Vec<_> = self
                    .chats
                    .iter()
                    .filter(|chat| {
                        editing
                            && self
                                .editor
                                .as_ref()
                                .and_then(|e| e.previous.as_ref())
                                .is_some_and(|s| s.target.chat.is_some())
                            && scope
                                .as_ref()
                                .is_ok_and(|scope| scope.matches_chat(&self.state, chat))
                    })
                    .cloned()
                    .collect();
                let mut choices = div().flex().flex_col().gap(ui_text::space(5.0)).child(
                    div()
                        .text_color(rgb(colors.muted))
                        .text_size(ui_text::text(9.0))
                        .child(ui_text::cased(
                            "Existing target · Click to explicitly bind this session",
                        )),
                );
                if targets.is_empty() && chats.is_empty() {
                    choices = choices.child(div().text_color(rgb(colors.gold)).child(scope.as_ref().err().cloned().unwrap_or("No live Codex or Claude session in this scope. Open one separately, complete a turn, then return here.".into())));
                }
                for target in targets {
                    choices = choices.child(self.button(
                        format!("{} · {}", target.harness.unwrap().program(), target.id),
                        Control::Target(ExistingTarget::Shell(target.id.clone())),
                        selected.as_ref() == Some(&target.id),
                        window,
                        cx,
                    ));
                }
                for chat in chats {
                    choices = choices.child(self.button(
                        format!(
                            "{} chat · {}",
                            schedules::chat_harness(chat.provider).program(),
                            chat.id
                        ),
                        Control::Target(ExistingTarget::LegacyChat(chat.id.clone())),
                        selected.as_ref() == Some(&chat.id),
                        window,
                        cx,
                    ));
                }
                if let Some(id) = selected {
                    choices = choices.child(
                        div()
                            .text_color(rgb(colors.muted))
                            .text_size(ui_text::text(9.0))
                            .child(ui_text::quiet(format!("PINNED TARGET  /  {id}"))),
                    );
                }
                form = form.child(choices);
            }
            let mut first_choices = div().flex().flex_wrap().gap(ui_text::space(5.0));
            for (seconds, label) in [
                (600, "In 10 min"),
                (1800, "In 30 min"),
                (3600, "In 1 hour"),
                (86400, "In 1 day"),
            ] {
                first_choices = first_choices.child(self.button(
                    ui_text::cased(label).to_string(),
                    Control::FirstIn(seconds),
                    first_quick == Some(seconds),
                    window,
                    cx,
                ));
            }
            first_choices = first_choices.child(self.button(
                ui_text::cased("Exact time…").to_string(),
                Control::ExactTime,
                exact_time,
                window,
                cx,
            ));
            form = form
                .child(
                    div()
                        .text_color(rgb(colors.muted))
                        .text_size(ui_text::text(9.0))
                        .child(ui_text::cased("First run")),
                )
                .child(first_choices)
                .child(
                    div()
                        .text_size(ui_text::text(10.0))
                        .child(ui_text::quiet(format!("LOCAL  {first_display}"))),
                );
            if exact_time {
                form = form.child(self.field(
                    2,
                    "Exact date / time · ISO with UTC offset",
                    window,
                    cx,
                ));
            }
            let mut presets = gpui_kit::base::RadioGroup::new("automation-repeat")
                .aria_label("Repeat cadence")
                .axis(gpui::Axis::Horizontal)
                .flex()
                .flex_wrap()
                .gap(ui_text::space(5.0));
            for (seconds, label) in [
                (0, "Once"),
                (3600, "Hourly"),
                (86400, "24 hours"),
                (604800, "7 days"),
                (u64::MAX, "Minutes…"),
            ] {
                presets = presets.child(self.button(
                    ui_text::cased(label).to_string(),
                    Control::Repeat(seconds),
                    repeat == seconds,
                    window,
                    cx,
                ));
            }
            form = form.child(presets);
            if repeat == u64::MAX {
                form = form.child(self.field(3, "Every N minutes · Minimum 5", window, cx));
            }
            form = form.child(div().text_color(rgb(colors.muted)).text_size(ui_text::text(9.0)).child("Repeats use elapsed time from the first run (fixed UTC cadence, including DST)."))
                .child(div().flex().gap(ui_text::space(6.0)).child(self.button(ui_text::cased(if editing {"Save changes"} else {"Create automation"}).to_string(),Control::Save,true,window,cx)).child(self.button(ui_text::cased("Cancel").to_string(),Control::Cancel,false,window,cx)));
            body = body.child(form);
        }
        if self.rows.is_empty() && !(native && self.editor.is_some()) {
            body = body.child(if native {
                controls::empty_state(
                    "calendar",
                    "No automations yet. Schedule a prompt to start a new project chat, or select an existing AI shell.",
                    colors,
                )
            } else {
                div()
                    .py(ui_text::space(12.0))
                    .text_color(rgb(colors.muted))
                    .child("No automations yet. Schedule a prompt to start a new project chat, or select an existing AI shell.")
            });
        }
        if native && !self.rows.is_empty() {
            body = body.child(
                controls::section_heading("Scheduled prompts", colors)
                    .px(ui_text::space(
                        controls::PANEL_INSET - controls::LIST_MARGIN,
                    ))
                    .pb_0(),
            );
        }
        for row in self.rows.clone() {
            let last = row
                .last_run
                .as_ref()
                .map(|r| {
                    format!(
                        "LAST  {} · {:?} · due {}",
                        format_display(r.observed_at),
                        r.outcome,
                        format_display(r.due_at)
                    )
                })
                .unwrap_or("LAST  —".into());
            let timing = match row.timing {
                Timing::Once { .. } => "Once".into(),
                Timing::Interval { seconds, .. } => format!("Every {} min", seconds / 60),
            };
            let mut item = div()
                .flex()
                .flex_col()
                .gap(ui_text::space(5.0))
                .py(ui_text::space(10.0))
                .border_b_1()
                .border_color(rgb(colors.divider))
                .map(|item| controls::native(item, |item| controls::card(item, colors)))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_between()
                        .gap(ui_text::space(8.0))
                        .child(format!("{}  /  {}", row.target.scope.label(), row.title))
                        .child(
                            div()
                                .text_color(rgb(if row.paused { colors.gold } else { colors.cyan }))
                                .child(ui_text::cased(if row.paused {
                                    "Paused"
                                } else if row.next_run.is_none() {
                                    "Complete"
                                } else {
                                    "Active"
                                })),
                        ),
                )
                .child(
                    div()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .child(format!(
                            "{} · {} · {}",
                            row.target.harness.program(),
                            if row.target.new_chat.is_some() {
                                "New project chat"
                            } else {
                                &row.target.shell_id
                            },
                            timing
                        )),
                )
                .child(div().text_size(ui_text::text(10.0)).child(format!(
                    "NEXT  {}",
                    row.next_run.map(format_display).unwrap_or("—".into())
                )))
                .child(
                    div()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .child(last),
                );
            if let Some(run) = &row.last_run {
                item = item.child(
                    div()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(
                            if matches!(
                                run.outcome,
                                schedules::Outcome::Failed
                                    | schedules::Outcome::Uncertain
                                    | schedules::Outcome::Missed
                            ) {
                                colors.gold
                            } else {
                                colors.muted
                            },
                        ))
                        .child(run.message.clone()),
                );
            }
            if let Some(fresh) = &row.target.new_chat {
                item = item.child(
                    div()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .child(format!(
                            "Permission: {:?} · Account: {} · Model: {} · Effort: {}{}",
                            fresh.approval_mode,
                            fresh.codex_account_id.as_deref().unwrap_or(
                                if fresh.provider == Provider::Claude {
                                    "Claude login"
                                } else {
                                    "System default"
                                }
                            ),
                            fresh.model.as_deref().unwrap_or("Default"),
                            fresh.effort.as_deref().unwrap_or("Default"),
                            if fresh.fast { " · Fast" } else { "" }
                        )),
                );
            }
            item = item.child(
                div()
                    .text_size(ui_text::text(10.0))
                    .child(row.prompt.clone()),
            );
            let mut actions = div()
                .flex()
                .flex_wrap()
                .gap(ui_text::space(5.0))
                .child(self.button(
                    ui_text::cased("Edit").to_string(),
                    Control::Edit(row.id.clone()),
                    false,
                    window,
                    cx,
                ))
                .child(self.button(
                    ui_text::cased(if row.paused { "Resume" } else { "Pause" }).to_string(),
                    Control::Pause(row.id.clone()),
                    false,
                    window,
                    cx,
                ))
                .child(
                    self.button(
                        ui_text::cased(if self.delete_confirm.as_ref() == Some(&row.id) {
                            "Confirm delete"
                        } else {
                            "Delete"
                        })
                        .to_string(),
                        Control::Delete(row.id.clone()),
                        false,
                        window,
                        cx,
                    ),
                );
            if let Some(id) = row
                .last_run
                .as_ref()
                .and_then(|run| run.created_chat_id.clone())
            {
                actions = actions.child(self.button(
                    "Open created chat".to_string(),
                    Control::OpenChat(id),
                    false,
                    window,
                    cx,
                ));
            }
            body = body.child(item.child(actions));
        }
        if !self.project_chats.is_empty() {
            body = body.child(
                div()
                    .text_color(rgb(colors.muted))
                    .child("Project chats · Includes earlier automation results"),
            );
            for chat in self.project_chats.clone() {
                body = body.child(self.button(
                    format!("Open {} · {}", chat.title, chat.id),
                    Control::OpenChat(chat.id),
                    false,
                    window,
                    cx,
                ));
            }
        }
        if let Some(error) = &self.error {
            self.control_focus.retain(|id, _| {
                self.controls
                    .iter()
                    .any(|control| control_id(control) == *id)
            });
            body = body.child(div().text_color(rgb(colors.gold)).child(error.clone()));
        }
        if let Some(error) = &cx.global::<schedules::RuntimeStatus>().0 {
            body = body.child(
                div()
                    .text_color(rgb(colors.gold))
                    .child(ui_text::quiet(format!("SCHEDULER ERROR  /  {error}"))),
            );
        }
        body = body.child(div().text_color(rgb(colors.muted)).text_size(ui_text::text(9.0)).when(native, |note| note.px(ui_text::space(controls::PANEL_INSET - controls::LIST_MARGIN))).child(if self.pending {ui_text::cased("Saving…")} else {"Runs while RiWork is open. Busy/blocked targets defer for up to 5 min; older runs are skipped. At most 4 attempts/min. Uncertain sends pause without retry. Edit and save a future run to resume after review. TAB / ENTER · CMD+S to save.".into()}));
        if let Some(control) = focused_control
            && let Some(index) = self.controls.iter().position(|c| c == &control)
        {
            self.active = index;
        }
        if let Some(header) = header {
            return controls::panel(colors)
                .id("schedule-panel")
                .track_focus(&self.focus)
                .key_context("Schedules")
                .capture_key_down(cx.listener(Self::key_down))
                .child(header)
                .child(
                    div()
                        .id("schedule-panel-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px(ui_text::space(controls::LIST_MARGIN))
                        .pb(ui_text::space(controls::PANEL_INSET))
                        .child(body.gap(ui_text::space(8.0))),
                )
                .into_any_element();
        }
        div()
            .id("schedule-panel")
            .size_full()
            .min_w_0()
            .track_focus(&self.focus)
            .key_context("Schedules")
            .capture_key_down(cx.listener(Self::key_down))
            .overflow_y_scroll()
            .p(ui_text::space(14.0))
            .bg(rgb(colors.bg))
            .text_color(rgb(colors.text))
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(11.0))
            .child(body)
            .into_any_element()
    }
}

fn selected_timing(repeat: u64, at: u64, minutes: &str) -> Result<Timing, String> {
    let timing = if repeat == 0 {
        Timing::Once { at }
    } else {
        let seconds = if repeat == u64::MAX {
            minutes
                .trim()
                .parse::<u64>()
                .ok()
                .and_then(|m| m.checked_mul(60))
                .ok_or("Enter repeat minutes")?
        } else {
            repeat
        };
        Timing::Interval { first: at, seconds }
    };
    timing.validate()?;
    Ok(timing)
}
fn optional_option(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| text.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{
        model::{ChatState, OrchestratorScope},
        testing::TestHost,
    };
    use std::fs;

    struct LegacyEditorFixture {
        sessions: SessionManager,
        host: TestHost,
        schedules: ScheduleStore,
        state: State,
        global: ChatInfo,
        project: ChatInfo,
    }
    impl LegacyEditorFixture {
        fn new() -> Self {
            let host = TestHost::new();
            let sessions = SessionManager::at(host.home.clone()).unwrap();
            let store = Store::open(host.home.clone()).unwrap();
            let root = host.home.join("project");
            fs::create_dir(&root).unwrap();
            let project = store.add_project(&root, None).unwrap();
            let mut global = host.new_chat(Provider::Codex);
            global.orchestrator = Some(OrchestratorScope::Global);
            let global = host.client().create(global).unwrap();
            let global = host.wait_for_state(&global.id, |state| *state == ChatState::Idle);
            let mut scoped = host.new_chat(Provider::Claude);
            scoped.cwd = project.root;
            scoped.project_id = Some(project.id.clone());
            scoped.orchestrator = Some(OrchestratorScope::Project {
                project_id: project.id,
            });
            let scoped = host.client().create(scoped).unwrap();
            let scoped = host.wait_for_state(&scoped.id, |state| *state == ChatState::Idle);
            let schedules = ScheduleStore::at(host.home.clone()).unwrap();
            Self {
                sessions,
                host,
                schedules,
                state: store.snapshot().unwrap(),
                global,
                project: scoped,
            }
        }
        fn edit_global(&self) -> Editor {
            let target = Target::bind_chat(Scope::App, &self.state, &self.global).unwrap();
            let row = self
                .schedules
                .save(
                    None,
                    "Legacy editor fixture".into(),
                    "Inert fixture draft".into(),
                    target.clone(),
                    Timing::Once { at: 1000 },
                    999,
                )
                .unwrap();
            let mut editor = Editor::new();
            editor.fresh = false;
            editor.scope = 0;
            editor.selection = Some(ExistingTarget::LegacyChat(self.global.id.clone()));
            editor.pinned = Some(target);
            editor.previous = Some(row);
            editor
        }
        fn save_selection(&self, editor: &mut Editor, scope: Scope) -> Target {
            assert!(!editor.fresh);
            assert!(editor.pinned_target().unwrap().is_none());
            let target = editor
                .selection
                .as_ref()
                .unwrap()
                .bind(scope, &self.state, &self.sessions, &self.schedules)
                .unwrap();
            let previous = editor.previous.as_ref().unwrap();
            let saved = self
                .schedules
                .save(
                    Some((&previous.id, previous.revision)),
                    "Edited legacy fixture".into(),
                    "Edited inert draft".into(),
                    target.clone(),
                    Timing::Once { at: 1000 },
                    999,
                )
                .unwrap();
            assert_eq!(saved.revision, previous.revision + 1);
            assert_eq!(saved.target, target);
            assert_eq!(saved.id, previous.id);
            editor.previous = Some(saved);
            target
        }
        fn ordinary_shell(&self) -> String {
            // A disposable cat pane and synthetic Claude identity exercise the
            // real shell binder without starting a provider or submitting input.
            let project = &self.state.projects[0];
            let shell = self
                .sessions
                .create(
                    project.id.clone(),
                    None,
                    project.root.clone(),
                    Some("/bin/cat".into()),
                )
                .unwrap();
            let registry = self.host.home.join("sessions.json");
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
            for entry in value["sessions"].as_array_mut().unwrap() {
                if entry["id"] == shell.id {
                    entry["harness"] = "claude".into();
                }
            }
            fs::write(registry, serde_json::to_vec(&value).unwrap()).unwrap();
            let hooks = self.host.home.join("agent-hooks/claude");
            fs::create_dir_all(&hooks).unwrap();
            fs::write(
                hooks.join(format!("{}.json", shell.id)),
                r#"{"session_id":"fixture-claude","turn_id":"fixture-turn","completed":true}"#,
            )
            .unwrap();
            shell.id
        }
    }
    impl Drop for LegacyEditorFixture {
        fn drop(&mut self) {
            self.sessions.kill_server();
        }
    }

    #[test]
    fn legacy_editor_same_chat_reselection_saves_a_chat_binding() {
        let fixture = LegacyEditorFixture::new();
        let mut editor = fixture.edit_global();
        let before = editor.pinned.clone().unwrap();
        // This is the same operation used by the displayed chat button.
        editor.select_target(ExistingTarget::LegacyChat(fixture.global.id.clone()));
        let rebound = fixture.save_selection(&mut editor, Scope::App);
        assert_eq!(rebound, before);
        assert!(
            rebound.chat.is_some() && rebound.new_chat.is_none() && rebound.shell_kind.is_none()
        );
        assert_eq!(fixture.schedules.list().unwrap().len(), 1);
        assert!(fixture.host.fake().commands().is_empty());
    }

    #[test]
    fn legacy_editor_scope_changes_rebind_only_the_matching_app_or_project_chat() {
        let fixture = LegacyEditorFixture::new();
        let mut editor = fixture.edit_global();
        let scope = Scope::Project {
            project_id: fixture.state.projects[0].id.clone(),
        };
        editor.select_scope(1);
        assert!(editor.selection.is_none() && editor.pinned.is_none());
        assert!(
            ExistingTarget::LegacyChat(fixture.global.id.clone())
                .bind(
                    scope.clone(),
                    &fixture.state,
                    &fixture.sessions,
                    &fixture.schedules
                )
                .is_err()
        );
        editor.select_target(ExistingTarget::LegacyChat(fixture.project.id.clone()));
        let rebound = fixture.save_selection(&mut editor, scope);
        assert!(rebound.matches_chat(&fixture.state, &fixture.project));
        assert_eq!(rebound.harness, HarnessKind::Claude);
        assert_eq!(
            rebound.chat.as_ref().unwrap().codex_account_id,
            fixture.project.codex_account_id
        );
        editor.select_scope(0);
        assert!(
            ExistingTarget::LegacyChat(fixture.project.id.clone())
                .bind(
                    Scope::App,
                    &fixture.state,
                    &fixture.sessions,
                    &fixture.schedules
                )
                .is_err()
        );
        editor.select_target(ExistingTarget::LegacyChat(fixture.global.id.clone()));
        let rebound = fixture.save_selection(&mut editor, Scope::App);
        assert!(rebound.matches_chat(&fixture.state, &fixture.global));
        assert_eq!(
            rebound.chat.as_ref().unwrap().codex_account_id,
            fixture.global.codex_account_id
        );
    }

    #[test]
    fn legacy_editor_ordinary_shell_selection_stays_explicit_and_never_falls_back() {
        let fixture = LegacyEditorFixture::new();
        let mut editor = fixture.edit_global();
        let id = fixture.ordinary_shell();
        let scope = Scope::Project {
            project_id: fixture.state.projects[0].id.clone(),
        };
        // The legacy terminal binder used by omitted CLI destinations is unchanged.
        assert!(Target::bind(scope.clone(), &fixture.state, &fixture.sessions, &id).is_err());
        assert!(
            ExistingTarget::LegacyChat(id.clone())
                .bind(
                    scope.clone(),
                    &fixture.state,
                    &fixture.sessions,
                    &fixture.schedules
                )
                .is_err()
        );
        assert!(
            ExistingTarget::Shell(fixture.global.id.clone())
                .bind(
                    Scope::App,
                    &fixture.state,
                    &fixture.sessions,
                    &fixture.schedules
                )
                .unwrap_err()
                .starts_with("unknown shell")
        );
        for choice in [
            ExistingTarget::Shell(uuid::Uuid::new_v4().to_string()),
            ExistingTarget::LegacyChat(uuid::Uuid::new_v4().to_string()),
        ] {
            assert!(
                choice
                    .bind(
                        scope.clone(),
                        &fixture.state,
                        &fixture.sessions,
                        &fixture.schedules
                    )
                    .is_err()
            );
        }
        let ordinary_chat = fixture.host.create(Provider::Claude);
        assert!(
            ExistingTarget::LegacyChat(ordinary_chat.id)
                .bind(
                    Scope::App,
                    &fixture.state,
                    &fixture.sessions,
                    &fixture.schedules
                )
                .is_err()
        );
        editor.select_scope(1);
        editor.select_target(ExistingTarget::Shell(id.clone()));
        let rebound = fixture.save_selection(&mut editor, scope);
        assert_eq!(rebound.shell_id, id);
        assert!(rebound.chat.is_none() && rebound.new_chat.is_none());
        assert_eq!(
            rebound.shell_kind,
            Some(crate::sessions::ShellKind::Project)
        );
        assert!(rebound.matches(&fixture.state, &fixture.sessions.get(&id).unwrap()));
    }

    #[test]
    fn editor_defaults_and_options_edit_keep_the_pinned_account_and_destination() {
        let mut editor = Editor::new();
        assert!(editor.fresh && !editor.fast);
        assert_eq!(editor.provider, Provider::Codex);
        assert_eq!(editor.permission, ApprovalMode::Supervised);
        assert!(optional_option(&editor.fields[4].text).is_none());
        assert!(optional_option(&editor.fields[5].text).is_none());
        let home = std::env::temp_dir().join(format!("riwork-editor-{}", uuid::Uuid::new_v4()));
        let root = home.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(home.clone()).unwrap();
        let project = store.add_project(&root, None).unwrap();
        let mut pinned = Target::bind_new_chat(
            &home,
            &store.snapshot().unwrap(),
            &project.id,
            Provider::Claude,
            None,
            None,
            false,
            ApprovalMode::Supervised,
            None,
        )
        .unwrap();
        // No account service is involved in an options-only edit.
        pinned.new_chat.as_mut().unwrap().codex_account_id = Some("pinned-account".into());
        editor.pinned = Some(pinned.clone());
        editor.fields[4] = Input::new(" fixture-model ".into());
        editor.fields[5] = Input::new(" high ".into());
        editor.fast = true;
        editor.permission = ApprovalMode::Plan;
        let edited = editor.pinned_target().unwrap().unwrap();
        let fresh = edited.new_chat.as_ref().unwrap();
        assert_eq!(fresh.model.as_deref(), Some("fixture-model"));
        assert_eq!(fresh.effort.as_deref(), Some("high"));
        assert!(fresh.fast);
        assert_eq!(fresh.approval_mode, ApprovalMode::Plan);
        assert_eq!(fresh.codex_account_id.as_deref(), Some("pinned-account"));
        assert_eq!(edited.shell_id, pinned.shell_id);
        assert_eq!(fresh.root, pinned.new_chat.as_ref().unwrap().root);
        editor.fields[4] = Input::default();
        editor.fields[5] = Input::default();
        editor.fast = false;
        let defaults = editor.pinned_target().unwrap().unwrap();
        assert!(defaults.new_chat.as_ref().unwrap().model.is_none());
        assert!(defaults.new_chat.as_ref().unwrap().effort.is_none());
        assert!(!defaults.new_chat.as_ref().unwrap().fast);
        editor.fields[4] = Input::new("bad\nmodel".into());
        assert!(editor.pinned_target().is_err());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn quick_first_run_choices_select_future_instants_without_editing_other_fields() {
        let mut editor = Editor::new();
        editor.fields[0] = Input::new("Title".into());
        editor.fields[1] = Input::new("Prompt".into());
        editor.selection = Some(ExistingTarget::Shell(
            "00000000-0000-4000-8000-000000000123".into(),
        ));
        let now = 1_790_000_000;
        for seconds in [600, 1800, 3600, 86400] {
            editor.first_in(seconds, now);
            assert_eq!(
                DateTime::parse_from_rfc3339(&editor.fields[2].text)
                    .unwrap()
                    .timestamp() as u64,
                now + seconds
            );
            assert_eq!(editor.fields[0].text, "Title");
            assert_eq!(editor.fields[1].text, "Prompt");
            assert_eq!(
                editor.selection.as_ref().map(ExistingTarget::id),
                Some("00000000-0000-4000-8000-000000000123")
            );
            assert!(!editor.exact_time);
        }
    }

    #[test]
    fn custom_zero_minutes_cannot_become_a_one_time_schedule() {
        assert!(selected_timing(u64::MAX, 1000, "0").is_err());
        assert!(selected_timing(u64::MAX, 1000, "4").is_err());
        assert!(selected_timing(u64::MAX, 1000, &u64::MAX.to_string()).is_err());
        assert_eq!(
            selected_timing(u64::MAX, 1000, "5").unwrap(),
            Timing::Interval {
                first: 1000,
                seconds: 300
            }
        );
        assert_eq!(
            selected_timing(0, 1000, "0").unwrap(),
            Timing::Once { at: 1000 }
        );
    }
}

pub fn format_time(unix: u64) -> String {
    DateTime::from_timestamp(unix as i64, 0)
        .map(|d| {
            d.with_timezone(&Local)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
        })
        .unwrap_or_default()
}
fn format_display(unix: u64) -> String {
    DateTime::from_timestamp(unix as i64, 0)
        .map(|d| {
            d.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S %:z")
                .to_string()
        })
        .unwrap_or_else(|| unix.to_string())
}

#[cfg(test)]
mod kit_control_tests {
    use super::*;
    use crate::form_input::{test_turn, test_window};
    use gpui::TestAppContext;
    use gpui_kit::test::TestWindowExt;

    // No SchedulePanel/Store/SessionManager: exercise its actual control factory only.
    struct Fixture {
        handles: Vec<FocusHandle>,
        calls: Vec<Control>,
        fast: bool,
        disabled: bool,
        permission: ApprovalMode,
    }
    impl Render for Fixture {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let actions = [
                Control::Target(ExistingTarget::Shell("same-id".into())),
                Control::Target(ExistingTarget::LegacyChat("same-id".into())),
                Control::Permission(ApprovalMode::Full),
                Control::Fast,
            ];
            div()
                .flex()
                .flex_col()
                .children(actions.into_iter().enumerate().map(|(index, action)| {
                    let selected = match action {
                        Control::Permission(value) => value == self.permission,
                        Control::Fast => self.fast,
                        _ => false,
                    };
                    let rendered_action = action.clone();
                    let listener = cx.listener(move |owner, _, _, cx| {
                        match action {
                            Control::Permission(value) => owner.permission = value,
                            Control::Fast => owner.fast = !owner.fast,
                            _ => {}
                        }
                        owner.calls.push(action.clone());
                        cx.notify();
                    });
                    action_control(
                        "Same display label".into(),
                        &rendered_action,
                        selected,
                        selected,
                        self.disabled,
                        &self.handles[index],
                        self.handles[index].is_focused(window),
                        theme::Palette::RIWORK,
                        listener,
                    )
                }))
        }
    }
    #[gpui::test]
    fn automation_controls_keep_typed_target_keyboard_radio_and_pending_boundaries(
        cx: &mut TestAppContext,
    ) {
        let (window, owner) = test_window(cx, |_, cx| Fixture {
            handles: (0..4).map(|_| cx.focus_handle()).collect(),
            calls: Vec::new(),
            fast: false,
            disabled: false,
            permission: ApprovalMode::Supervised,
        });
        let shell = Control::Target(ExistingTarget::Shell("same-id".into()));
        let chat = Control::Target(ExistingTarget::LegacyChat("same-id".into()));
        assert_ne!(control_id(&shell), control_id(&chat));
        test_turn(cx, window, |window, app| {
            window.click(control_id(&shell), app)
        });
        test_turn(cx, window, |window, app| window.press("enter", app));
        test_turn(cx, window, |window, app| {
            window.click(control_id(&chat), app)
        });
        test_turn(cx, window, |window, app| {
            window.click(control_id(&Control::Permission(ApprovalMode::Full)), app)
        });
        // Already selected Radio has no second domain callback.
        test_turn(cx, window, |window, app| window.press("space", app));
        test_turn(cx, window, |window, app| {
            assert_eq!(
                owner.read(app).calls,
                vec![
                    shell.clone(),
                    shell,
                    chat,
                    Control::Permission(ApprovalMode::Full)
                ]
            );
            let radio = crate::form_input::test_ax_node(
                window,
                app,
                control_id(&Control::Permission(ApprovalMode::Full)),
            );
            assert_eq!(radio.role(), gpui::Role::RadioButton);
            assert_eq!(radio.label(), Some("Same display label"));
            assert_eq!(radio.toggled(), Some(gpui::accesskit::Toggled::True));
            window.click(control_id(&Control::Fast), app);
        });
        test_turn(cx, window, |window, app| window.press("space", app));
        test_turn(cx, window, |_, app| {
            owner.update(app, |owner, cx| {
                owner.disabled = true;
                cx.notify();
            })
        });
        test_turn(cx, window, |window, app| {
            window.click(control_id(&Control::Fast), app)
        });
        test_turn(cx, window, |window, app| window.press("enter", app));
        test_turn(cx, window, |window, app| {
            assert!(!owner.read(app).fast);
            assert_eq!(owner.read(app).calls.len(), 6);
            for action in [Control::Permission(ApprovalMode::Full), Control::Fast] {
                let node = crate::form_input::test_ax_node(window, app, control_id(&action));
                assert!(node.is_disabled());
                assert!(!node.supports_action(gpui::accesskit::Action::Click));
            }
        });
    }
}
