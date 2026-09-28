//! Compact scheduling tab, using RiWork's existing GPUI input and palette.
use crate::{
    project_settings::{Input, impl_input_handler, input_content},
    schedules::{self, Schedule, ScheduleStore, Scope, Target, Timing},
    sessions::{SessionManager, ShellSession},
    store::{State, Store},
    theme, utf16_to_byte,
};
use chrono::{DateTime, Local};
use gpui::{
    AnyElement, Bounds, Context, EntityInputHandler, FocusHandle, IntoElement, KeyDownEvent,
    Pixels, Point, Render, UTF16Selection, Window, div, prelude::*, px, rgb,
};
use std::{ops::Range, time::Duration};

#[derive(Clone, PartialEq, Eq)]
enum Control {
    Field(usize),
    Scope(usize),
    Target(String),
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
struct Editor {
    previous: Option<Schedule>,
    scope: usize,
    target_id: Option<String>,
    pinned: Option<Target>,
    repeat: u64,
    first_quick: Option<u64>,
    exact_time: bool,
    fields: [Input; 4],
}
impl Editor {
    fn new() -> Self {
        Self {
            previous: None,
            scope: 1,
            target_id: None,
            pinned: None,
            repeat: 0,
            first_quick: Some(600),
            exact_time: false,
            fields: [
                Input::default(),
                Input::default(),
                Input::new(format_time(schedules::now() + 600)),
                Input::new("60".into()),
            ],
        }
    }

    fn first_in(&mut self, seconds: u64, now: u64) {
        self.fields[2] = Input::new(format_time(now + seconds));
        self.first_quick = Some(seconds);
    }
}
pub struct SchedulePanel {
    store: Store,
    sessions: SessionManager,
    schedules: ScheduleStore,
    project_id: String,
    workspace_id: Option<String>,
    state: State,
    targets: Vec<ShellSession>,
    rows: Vec<Schedule>,
    editor: Option<Editor>,
    controls: Vec<Control>,
    active: usize,
    focus: FocusHandle,
    error: Option<String>,
    pending: bool,
    refreshing: bool,
    delete_confirm: Option<String>,
    dummy: Input,
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
            rows: vec![],
            editor: None,
            controls: vec![],
            active: 0,
            focus: cx.focus_handle(),
            error: None,
            pending: false,
            refreshing: false,
            delete_confirm: None,
            dummy: Input::default(),
        };
        panel.refresh(cx);
        panel
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
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
            Ok::<_, String>((store.snapshot()?, sessions.list()?, schedules.list()?))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |panel, cx| {
                panel.refreshing = false;
                match result {
                    Ok((state, targets, rows)) => {
                        panel.state = state;
                        panel.targets = targets;
                        panel.rows = rows
                            .into_iter()
                            .filter(|s| s.target.scope.visible(&panel.project_id))
                            .collect();
                    }
                    Err(e) => panel.error = Some(e),
                };
                cx.notify();
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
    fn input(&self) -> &Input {
        if let Some(Control::Field(i)) = self.controls.get(self.active)
            && let Some(e) = &self.editor
        {
            &e.fields[*i]
        } else {
            &self.dummy
        }
    }
    fn input_mut(&mut self) -> &mut Input {
        if let Some(Control::Field(i)) = self.controls.get(self.active)
            && let Some(e) = &mut self.editor
        {
            &mut e.fields[*i]
        } else {
            &mut self.dummy
        }
    }
    fn accepts_input(&self) -> bool {
        !self.pending
            && self.editor.is_some()
            && matches!(self.controls.get(self.active), Some(Control::Field(_)))
    }
    fn edited(&mut self, cx: &mut Context<Self>) {
        if matches!(self.controls.get(self.active), Some(Control::Field(2)))
            && let Some(editor) = &mut self.editor
        {
            editor.first_quick = None;
        }
        self.error = None;
        cx.notify();
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
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
                self.active = 0;
                self.delete_confirm = None;
            }
            "s" if event.keystroke.modifiers.platform && self.editor.is_some() => self.save(cx),
            "enter" | "space" if !self.accepts_input() => {
                if let Some(action) = self.controls.get(self.active).cloned() {
                    self.perform(action, window, cx);
                }
            }
            "enter" if self.editor.is_some() => self.save(cx),
            _ if self.accepts_input() => {
                let before = self.input().text.clone();
                if !self.input_mut().key(event, cx) {
                    return;
                }
                if self.input().text != before {
                    self.edited(cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
    fn perform(&mut self, control: Control, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        self.focus.focus(window, cx);
        self.error = None;
        let deleting = matches!(control, Control::Delete(_));
        match control {
            Control::Field(_) => {}
            Control::New => {
                self.editor = Some(Editor::new());
                self.delete_confirm = None;
            }
            Control::Cancel => {
                self.editor = None;
                self.active = 0;
                self.delete_confirm = None;
            }
            Control::Scope(scope) => {
                if let Some(e) = &mut self.editor {
                    e.scope = scope;
                    e.target_id = None;
                    e.pinned = None;
                }
            }
            Control::Target(id) => {
                if let Some(e) = &mut self.editor {
                    e.target_id = Some(id);
                    e.pinned = None;
                }
            }
            Control::Workspace(id) => {
                self.workspace_id = Some(id);
                if let Some(e) = &mut self.editor {
                    e.target_id = None;
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
                    self.editor = Some(Editor {
                        fields: [
                            Input::new(s.title.clone()),
                            Input::new(s.prompt.clone()),
                            Input::new(format_time(
                                s.next_run
                                    .filter(|n| *n > schedules::now())
                                    .unwrap_or(schedules::now() + 600),
                            )),
                            Input::new((repeat.max(300) / 60).to_string()),
                        ],
                        scope,
                        target_id: Some(s.target.shell_id.clone()),
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
        let result = (|| {
            let e = self.editor.as_ref().ok_or("No schedule editor")?;
            let parsed = DateTime::parse_from_rfc3339(e.fields[2].text.trim())
                .map_err(|_| "Use YYYY-MM-DDTHH:MM:SS+HH:MM with an explicit UTC offset")?;
            let at = u64::try_from(parsed.timestamp()).map_err(|_| "Date must be after 1970")?;
            let timing = selected_timing(e.repeat, at, &e.fields[3].text)?;
            let scope = self.scope(e.scope)?;
            let id = e
                .target_id
                .clone()
                .ok_or("Choose an existing target session")?;
            Ok::<_, String>((
                e.previous.as_ref().map(|s| (s.id.clone(), s.revision)),
                e.fields[0].text.clone(),
                e.fields[1].text.clone(),
                scope,
                id,
                e.pinned.clone(),
                timing,
            ))
        })();
        let (previous, title, prompt, scope, id, pinned, timing) = match result {
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
        let work = cx.background_executor().spawn(async move {
            let state = store.snapshot()?;
            let target = match pinned {
                Some(target) => target,
                None => Target::bind(scope, &state, &sessions, &id)?,
            };
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
                        panel.editor = None;
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
        self.controls.push(action.clone());
        div()
            .id(format!("schedule-control-{index}"))
            .px(px(8.0))
            .py(px(5.0))
            .cursor_pointer()
            .border_1()
            .border_color(rgb(
                if self.active == index && self.focus.is_focused(window) {
                    colors.gold
                } else if selected {
                    colors.cyan
                } else {
                    colors.divider
                },
            ))
            .bg(rgb(if selected {
                colors.panel_active
            } else {
                colors.bg
            }))
            .text_color(rgb(if selected { colors.cyan } else { colors.text }))
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .child(label)
            .on_click(cx.listener(move |panel, _, window, cx| {
                panel.active = index;
                panel.perform(action.clone(), window, cx);
            }))
            .into_any_element()
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
        let input = &self.editor.as_ref().unwrap().fields[index];
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .min_w_0()
            .w_full()
            .child(
                div()
                    .text_size(px(9.0))
                    .text_color(rgb(colors.muted))
                    .child(label.to_owned()),
            )
            .child(
                div()
                    .id(format!("schedule-field-{index}"))
                    .min_w_0()
                    .border_1()
                    .border_color(rgb(colors.divider))
                    .child(input_content(
                        input,
                        self.active == control && self.focus.is_focused(window),
                        "",
                        &self.focus,
                        cx.entity(),
                        colors,
                    ))
                    .on_click(cx.listener(move |panel, _, window, cx| {
                        panel.active = control;
                        panel.focus.focus(window, cx);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}
impl Render for SchedulePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let focused_control = self.controls.get(self.active).cloned();
        self.controls.clear();
        let mut body = div()
            .w_full()
            .min_w_0()
            .max_w(px(800.0))
            .flex()
            .flex_col()
            .gap(px(12.0));
        body = body.child(
            div()
                .flex()
                .flex_wrap()
                .justify_between()
                .items_center()
                .gap(px(8.0))
                .child(div().text_color(rgb(colors.cyan)).child("SCHEDULES"))
                .child(self.button("+ SCHEDULE", Control::New, false, window, cx)),
        );
        body = body.child(div().text_color(rgb(colors.muted)).text_size(px(10.0)).child("App → global orchestrator · Project → project orchestrator · Workspace → selected worktree worker"));
        if let Some(editor) = &self.editor {
            let scope_index = editor.scope;
            let repeat = editor.repeat;
            let selected = editor.target_id.clone();
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
                .gap(px(10.0))
                .p(px(12.0))
                .border_1()
                .border_color(rgb(colors.divider))
                .bg(rgb(colors.panel));
            let mut scopes = div().flex().flex_wrap().gap(px(5.0));
            for (i, label) in ["APP", "PROJECT", "WORKSPACE"].iter().enumerate() {
                scopes = scopes.child(self.button(
                    *label,
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
                let mut choices = div().flex().flex_wrap().gap(px(5.0));
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
                            .text_size(px(9.0))
                            .child("WORKSPACE / WORKTREE"),
                    )
                    .child(choices);
            }
            form = form
                .child(self.field(0, "TITLE", window, cx))
                .child(self.field(1, "PROMPT · SINGLE LINE", window, cx));
            let scope = self.scope(scope_index);
            let targets: Vec<_> = self
                .targets
                .iter()
                .filter(|s| {
                    s.alive
                        && s.harness.is_some()
                        && scope
                            .as_ref()
                            .is_ok_and(|scope| scope.matches(&self.state, s))
                })
                .cloned()
                .collect();
            let mut choices = div().flex().flex_col().gap(px(5.0)).child(
                div()
                    .text_color(rgb(colors.muted))
                    .text_size(px(9.0))
                    .child("EXISTING TARGET · CLICK TO EXPLICITLY BIND THIS SESSION"),
            );
            if targets.is_empty() {
                choices = choices.child(div().text_color(rgb(colors.gold)).child(scope.as_ref().err().cloned().unwrap_or("No live harness in this scope. Open one separately, complete a turn, then return here.".into())));
            }
            for target in targets {
                choices = choices.child(self.button(
                    format!("{} · {}", target.harness.unwrap().program(), target.id),
                    Control::Target(target.id.clone()),
                    selected.as_ref() == Some(&target.id),
                    window,
                    cx,
                ));
            }
            if let Some(id) = selected {
                choices = choices.child(
                    div()
                        .text_color(rgb(colors.muted))
                        .text_size(px(9.0))
                        .child(format!("PINNED TARGET  /  {id}")),
                );
            }
            let mut first_choices = div().flex().flex_wrap().gap(px(5.0));
            for (seconds, label) in [
                (600, "IN 10 MIN"),
                (1800, "IN 30 MIN"),
                (3600, "IN 1 HOUR"),
                (86400, "IN 1 DAY"),
            ] {
                first_choices = first_choices.child(self.button(
                    label,
                    Control::FirstIn(seconds),
                    first_quick == Some(seconds),
                    window,
                    cx,
                ));
            }
            first_choices = first_choices.child(self.button(
                "EXACT TIME…",
                Control::ExactTime,
                exact_time,
                window,
                cx,
            ));
            form = form
                .child(choices)
                .child(
                    div()
                        .text_color(rgb(colors.muted))
                        .text_size(px(9.0))
                        .child("FIRST RUN"),
                )
                .child(first_choices)
                .child(
                    div()
                        .text_size(px(10.0))
                        .child(format!("LOCAL  {first_display}")),
                );
            if exact_time {
                form = form.child(self.field(
                    2,
                    "EXACT DATE / TIME · ISO WITH UTC OFFSET",
                    window,
                    cx,
                ));
            }
            let mut presets = div().flex().flex_wrap().gap(px(5.0));
            for (seconds, label) in [
                (0, "ONCE"),
                (3600, "HOURLY"),
                (86400, "24 HOURS"),
                (604800, "7 DAYS"),
                (u64::MAX, "MINUTES…"),
            ] {
                presets = presets.child(self.button(
                    label,
                    Control::Repeat(seconds),
                    repeat == seconds,
                    window,
                    cx,
                ));
            }
            form = form.child(presets);
            if repeat == u64::MAX {
                form = form.child(self.field(3, "EVERY N MINUTES · MINIMUM 5", window, cx));
            }
            form = form.child(div().text_color(rgb(colors.muted)).text_size(px(9.0)).child("Repeats use elapsed time from the first run (fixed UTC cadence, including DST)."))
                .child(div().flex().gap(px(6.0)).child(self.button(if editing {"SAVE CHANGES"} else {"CREATE SCHEDULE"},Control::Save,true,window,cx)).child(self.button("CANCEL",Control::Cancel,false,window,cx)));
            body = body.child(form);
        }
        if self.rows.is_empty() {
            body = body.child(
                div()
                    .py(px(12.0))
                    .text_color(rgb(colors.muted))
                    .child("No schedules. Create a prompt when you are ready."),
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
                .gap(px(5.0))
                .py(px(10.0))
                .border_b_1()
                .border_color(rgb(colors.divider))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_between()
                        .gap(px(8.0))
                        .child(format!("{}  /  {}", row.target.scope.label(), row.title))
                        .child(
                            div()
                                .text_color(rgb(if row.paused { colors.gold } else { colors.cyan }))
                                .child(if row.paused {
                                    "PAUSED"
                                } else if row.next_run.is_none() {
                                    "COMPLETE"
                                } else {
                                    "ACTIVE"
                                }),
                        ),
                )
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(colors.muted))
                        .child(format!(
                            "{} · {} · {}",
                            row.target.harness.program(),
                            row.target.shell_id,
                            timing
                        )),
                )
                .child(div().text_size(px(10.0)).child(format!(
                    "NEXT  {}",
                    row.next_run.map(format_display).unwrap_or("—".into())
                )))
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(colors.muted))
                        .child(last),
                );
            if let Some(run) = &row.last_run {
                item = item.child(
                    div()
                        .text_size(px(10.0))
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
            let actions = div()
                .flex()
                .flex_wrap()
                .gap(px(5.0))
                .child(self.button("EDIT", Control::Edit(row.id.clone()), false, window, cx))
                .child(self.button(
                    if row.paused { "RESUME" } else { "PAUSE" },
                    Control::Pause(row.id.clone()),
                    false,
                    window,
                    cx,
                ))
                .child(self.button(
                    if self.delete_confirm.as_ref() == Some(&row.id) {
                        "CONFIRM DELETE"
                    } else {
                        "DELETE"
                    },
                    Control::Delete(row.id.clone()),
                    false,
                    window,
                    cx,
                ));
            body = body.child(item.child(actions));
        }
        if let Some(error) = &self.error {
            body = body.child(div().text_color(rgb(colors.gold)).child(error.clone()));
        }
        if let Some(error) = &cx.global::<schedules::RuntimeStatus>().0 {
            body = body.child(
                div()
                    .text_color(rgb(colors.gold))
                    .child(format!("SCHEDULER ERROR  /  {error}")),
            );
        }
        body = body.child(div().text_color(rgb(colors.muted)).text_size(px(9.0)).child(if self.pending {"SAVING…"} else {"Runs while RiWork is open. Busy/blocked targets defer for up to 5 min; older runs are skipped. At most 4 attempts/min. Uncertain sends pause without retry. Edit and save a future run to resume after review. TAB / ENTER · CMD+S to save."}));
        if let Some(control) = focused_control
            && let Some(index) = self.controls.iter().position(|c| c == &control)
        {
            self.active = index;
        }
        div()
            .id("schedule-panel")
            .size_full()
            .min_w_0()
            .track_focus(&self.focus)
            .key_context("Schedules")
            .on_key_down(cx.listener(Self::key_down))
            .overflow_y_scroll()
            .p(px(14.0))
            .bg(rgb(colors.bg))
            .text_color(rgb(colors.text))
            .font_family("Menlo")
            .text_size(px(11.0))
            .child(body)
    }
}
impl_input_handler!(SchedulePanel);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_first_run_choices_select_future_instants_without_editing_other_fields() {
        let mut editor = Editor::new();
        editor.fields[0] = Input::new("Title".into());
        editor.fields[1] = Input::new("Prompt".into());
        editor.target_id = Some("00000000-0000-4000-8000-000000000123".into());
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
                editor.target_id.as_deref(),
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
