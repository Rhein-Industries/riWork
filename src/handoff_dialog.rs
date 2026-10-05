//! The small dialog behind **Hand off…** in a terminal agent tab's menu and a chat tab's ⋯
//! menu: where the conversation goes (a shell or a chat), which agent, which model and Codex
//! account, whether it is passed on as a transcript or as a summary the agent writes, and a
//! note. The work is `crate::handoff`; this runs it off the UI thread, says what it is doing
//! ("Writing summary…"), and tells the window when the new tab is there to open.

use std::{ops::Range, path::PathBuf, sync::Arc};

use gpui::{
    AnyElement, Bounds, Context, EntityInputHandler, EventEmitter, FocusHandle, IntoElement,
    KeyDownEvent, MouseButton, Pixels, Point, Render, UTF16Selection, Window, div, prelude::*, rgb,
};

use crate::{
    codex_accounts,
    controls::{self, Button},
    handoff::{self, Context as Passing, Kind, Outcome, Request},
    project_settings::{Input, impl_input_handler, input_content},
    sessions::HarnessKind,
    theme, ui_text, utf16_to_byte,
};

/// How the dialog reaches the chat host: it starts it when it is not running.
pub type Ensure = Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>;

pub enum HandoffEvent {
    /// Closed without handing anything over.
    Closed,
    /// Hidden while the work goes on: the agent that is asked for a summary may be waiting
    /// for an approval in its own tab, which a dialog over the window would keep from
    /// being answered. The window keeps the dialog and hears the rest from it.
    Detached,
    /// What a hidden dialog's work is doing now.
    Progress(String),
    /// A hidden dialog's work failed. (A shown one says so itself.)
    Failed(String),
    /// The new shell or chat exists; the window opens its tab.
    Done(Box<Outcome>),
}

/// The longest model name and note the dialog accepts, as the command line does.
const MAX_MODEL_CHARS: usize = 100;
const MAX_NOTE_CHARS: usize = 2000;

/// What the dialog hands off from.
pub struct HandoffSource {
    /// The shell's or chat's id.
    pub id: String,
    /// What to call it: "Codex chat "Fix the build" (1234abcd)".
    pub label: String,
    /// A Codex or Claude agent that can be asked for a summary.
    pub askable: bool,
    /// What the target is by default: the source's own kind and agent.
    pub kind: Kind,
    pub provider: HarnessKind,
}

/// The lines of the dialog, top to bottom, and the two buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Target,
    Agent,
    Model,
    Account,
    Context,
    Note,
    Cancel,
    Start,
}

/// What the dialog's choices come to, apart from the window: which lines there are, what
/// each choice does to the others, and the request they make.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Choices {
    kind: Kind,
    provider: HarnessKind,
    context: Passing,
    askable: bool,
}

impl Choices {
    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Target, Row::Agent, Row::Model];
        if self.provider == HarnessKind::Codex {
            rows.push(Row::Account);
        }
        if self.askable {
            rows.push(Row::Context);
        }
        rows.extend([Row::Note, Row::Cancel, Row::Start]);
        rows
    }

    /// The agents the target can be: chats run Codex and Claude, terminals also Grok.
    fn agents(&self) -> &'static [HarnessKind] {
        match self.kind {
            Kind::Chat => &[HarnessKind::Codex, HarnessKind::Claude],
            Kind::Shell => &[HarnessKind::Codex, HarnessKind::Claude, HarnessKind::Grok],
        }
    }

    fn set_kind(&mut self, kind: Kind) {
        self.kind = kind;
        if !self.agents().contains(&self.provider) {
            self.provider = HarnessKind::Codex;
        }
    }

    /// The model suggestions for the agent; a model is a free text, and these are only
    /// what saves typing.
    fn suggestions(&self) -> &'static [&'static str] {
        match self.provider {
            HarnessKind::Codex => &["gpt-5"],
            HarnessKind::Claude => &["opus", "sonnet", "haiku"],
            HarnessKind::Grok => &[],
        }
    }
}

/// The words of one choice among several.
fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Shell => "Shell",
        Kind::Chat => "Chat",
    }
}

fn agent_label(harness: HarnessKind) -> &'static str {
    handoff::agent_name(harness)
}

fn context_label(context: Passing) -> &'static str {
    match context {
        Passing::Transcript => "Transcript",
        Passing::Summary => "Summary",
    }
}

/// An account to choose from: what it is called, and what the request names it by (none:
/// the project's own, as a new tab would have it).
type AccountChoice = (String, Option<String>);

/// The accounts the dialog offers: the project's own, the system's, and the saved ones
/// that can be used (from the list Settings keeps; nothing is run to make it).
fn account_choices(home: &std::path::Path) -> Vec<AccountChoice> {
    let mut choices: Vec<AccountChoice> = vec![
        ("Project default".into(), None),
        (
            "System default".into(),
            Some(codex_accounts::SYSTEM_DEFAULT_ID.to_owned()),
        ),
    ];
    if let Ok(accounts) = codex_accounts::cached_accounts(home) {
        choices.extend(
            accounts
                .into_iter()
                .filter(|account| account.available && !account.is_system_default)
                .map(|account| (account.label, Some(account.id))),
        );
    }
    choices
}

pub struct HandoffDialog {
    source: HandoffSource,
    home: PathBuf,
    ensure: Ensure,
    choices: Choices,
    accounts: Vec<AccountChoice>,
    account: usize,
    model: Input,
    note: Input,
    active: Row,
    /// What the work in flight is doing; the dialog ignores input meanwhile.
    busy: Option<String>,
    /// The dialog is hidden and the work goes on (see `HandoffEvent::Detached`).
    detached: bool,
    error: Option<String>,
    focus: FocusHandle,
}

impl EventEmitter<HandoffEvent> for HandoffDialog {}

impl HandoffDialog {
    pub fn new(
        source: HandoffSource,
        home: PathBuf,
        ensure: Ensure,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe_global::<theme::Appearance>(|_, cx| cx.notify())
            .detach();
        let choices = Choices {
            kind: source.kind,
            provider: source.provider,
            context: Passing::Transcript,
            askable: source.askable,
        };
        let accounts = account_choices(&home);
        Self {
            source,
            home,
            ensure,
            choices,
            accounts,
            account: 0,
            model: Input::default(),
            note: Input::default(),
            active: Row::Model,
            busy: None,
            detached: false,
            error: None,
            focus: cx.focus_handle(),
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    fn accepts_input(&self) -> bool {
        self.busy.is_none() && matches!(self.active, Row::Model | Row::Note)
    }

    fn input(&self) -> &Input {
        match self.active {
            Row::Note => &self.note,
            _ => &self.model,
        }
    }

    fn input_mut(&mut self) -> &mut Input {
        match self.active {
            Row::Note => &mut self.note,
            _ => &mut self.model,
        }
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    /// The row `step` lines on (or back) from the active one, around.
    fn move_active(&mut self, step: isize) {
        let rows = self.choices.rows();
        let at = rows
            .iter()
            .position(|row| *row == self.active)
            .unwrap_or_default() as isize;
        self.active = rows[(at + step).rem_euclid(rows.len() as isize) as usize];
    }

    /// Changes the choice of the active row by `step` places, around.
    fn step_choice(&mut self, step: isize) {
        fn around<T: Copy + PartialEq>(options: &[T], current: T, step: isize) -> T {
            let at = options
                .iter()
                .position(|o| *o == current)
                .unwrap_or_default() as isize;
            options[(at + step).rem_euclid(options.len() as isize) as usize]
        }
        match self.active {
            Row::Target => {
                let kind = around(&[Kind::Shell, Kind::Chat], self.choices.kind, step);
                self.choices.set_kind(kind);
            }
            Row::Agent => {
                self.choices.provider = around(self.choices.agents(), self.choices.provider, step)
            }
            Row::Account => {
                self.account =
                    (self.account as isize + step).rem_euclid(self.accounts.len() as isize) as usize
            }
            Row::Context => {
                self.choices.context = around(
                    &[Passing::Transcript, Passing::Summary],
                    self.choices.context,
                    step,
                )
            }
            _ => {}
        }
        self.error = None;
    }

    fn pick(&mut self, row: Row, apply: impl FnOnce(&mut Self), cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.active = row;
        apply(self);
        self.error = None;
        cx.notify();
    }

    fn text(input: &Input) -> Option<String> {
        let text = input.text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }

    fn request(&self, source: handoff::Source) -> Request {
        Request {
            source,
            kind: self.choices.kind,
            provider: self.choices.provider,
            model: Self::text(&self.model),
            effort: None,
            account: if self.choices.provider == HarnessKind::Codex {
                self.accounts
                    .get(self.account)
                    .and_then(|(_, id)| id.clone())
            } else {
                None
            },
            mode: None,
            context: if self.choices.askable {
                self.choices.context
            } else {
                Passing::Transcript
            },
            note: Self::text(&self.note),
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.error = None;
        if self.model.text.chars().count() > MAX_MODEL_CHARS {
            self.error = Some(format!(
                "A model name is at most {MAX_MODEL_CHARS} characters."
            ));
            return;
        }
        if self.note.text.chars().count() > MAX_NOTE_CHARS {
            self.error = Some(format!("A note is at most {MAX_NOTE_CHARS} characters."));
            return;
        }
        self.busy = Some(
            match self.choices.context {
                Passing::Summary if self.choices.askable => "Asking for a summary…",
                _ => "Reading the conversation…",
            }
            .to_owned(),
        );
        let (home, ensure, id) = (
            self.home.clone(),
            self.ensure.clone(),
            self.source.id.clone(),
        );
        let shaped = self.request(placeholder_source());
        let (steps, progress) = async_channel::unbounded::<String>();
        let work = cx.background_executor().spawn(async move {
            let source = handoff::resolve_source(&home, &id)?;
            let env = handoff::Env {
                home: &home,
                ensure: &*ensure,
                account: &codex_accounts::resolve_account,
                timing: handoff::summary::Timing::DEFAULT,
            };
            handoff::run(&env, Request { source, ..shaped }, &|step| {
                let _ = steps.try_send(step.to_owned());
            })
        });
        cx.spawn(async move |this, cx| {
            while let Ok(step) = progress.recv().await {
                if this
                    .update(cx, |dialog, cx| {
                        if dialog.detached {
                            cx.emit(HandoffEvent::Progress(step.clone()));
                        }
                        dialog.busy = Some(step);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |dialog, cx| {
                dialog.busy = None;
                match result {
                    Ok(outcome) => cx.emit(HandoffEvent::Done(Box::new(outcome))),
                    Err(error) if dialog.detached => cx.emit(HandoffEvent::Failed(error)),
                    Err(error) => dialog.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn press(&mut self, row: Row, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            if row == Row::Cancel {
                self.detach(cx);
            }
            return;
        }
        match row {
            Row::Cancel => cx.emit(HandoffEvent::Closed),
            _ => self.submit(cx),
        }
    }

    /// Hides the dialog and lets the work finish: it cannot be taken back, and the window
    /// is needed meanwhile.
    fn detach(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() && !self.detached {
            self.detached = true;
            cx.emit(HandoffEvent::Detached);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let handled = match event.keystroke.key.as_str() {
            "escape" => {
                if self.busy.is_none() {
                    cx.emit(HandoffEvent::Closed);
                } else {
                    self.detach(cx);
                }
                true
            }
            _ if self.busy.is_some() => true,
            "enter" | "return" => {
                self.press(self.active, cx);
                true
            }
            "tab" => {
                self.move_active(if event.keystroke.modifiers.shift {
                    -1
                } else {
                    1
                });
                true
            }
            "up" | "down" => {
                self.move_active(if event.keystroke.key == "up" { -1 } else { 1 });
                true
            }
            "left" | "right"
                if !self.accepts_input() && !matches!(self.active, Row::Cancel | Row::Start) =>
            {
                self.step_choice(if event.keystroke.key == "left" { -1 } else { 1 });
                true
            }
            "space" if !self.accepts_input() => {
                match self.active {
                    Row::Cancel | Row::Start => self.press(self.active, cx),
                    _ => self.step_choice(1),
                }
                true
            }
            _ => {
                let handled = self.accepts_input() && self.input_mut().key(event, cx);
                if handled {
                    self.edited(cx);
                }
                handled
            }
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    // ---- Drawing -------------------------------------------------------------------------

    fn chip(
        &self,
        id: (&'static str, usize),
        label: String,
        selected: bool,
        enabled: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        div()
            .id(id)
            .px(ui_text::space(10.0))
            .py(ui_text::space(5.0))
            .border_1()
            .border_color(rgb(if selected {
                colors.cyan
            } else {
                colors.divider
            }))
            .bg(rgb(if selected {
                colors.panel_active
            } else {
                colors.panel
            }))
            .text_color(rgb(if !enabled {
                colors.divider
            } else if selected {
                colors.cyan
            } else {
                colors.muted
            }))
            // Native: a segment of the line's segmented control, raised when chosen.
            .map(|chip| {
                controls::native(chip, |chip| {
                    controls::segment(chip, selected, colors)
                        .when(!enabled, |chip| chip.text_color(rgb(colors.divider)))
                        .when(enabled && !selected, |chip| {
                            chip.hover(move |style| {
                                style.bg(rgb(controls::segment_hover(false, colors)))
                            })
                        })
                })
            })
            .when(enabled, |chip| {
                chip.cursor_pointer()
                    .on_click(cx.listener(move |dialog, _, _, cx| on_click(dialog, cx)))
            })
            .child(label)
            .into_any_element()
    }

    /// A line of the dialog: its name, and what is on it.
    fn line(
        &self,
        row: Row,
        name: &'static str,
        content: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let active = self.active == row && self.busy.is_none();
        div()
            .id(("handoff-line", row as usize))
            .flex()
            .flex_col()
            .gap(ui_text::space(4.0))
            .child(
                div()
                    .text_size(ui_text::text(9.0))
                    .text_color(rgb(if active { colors.cyan } else { colors.muted }))
                    // Native names a line like a form's label: semibold, in sentence case.
                    .when(ui_text::is_native(), |name| {
                        name.text_size(ui_text::text(10.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                    })
                    .child(ui_text::cased(name)),
            )
            .child(content)
            .on_click(cx.listener(move |dialog, _, window, cx| {
                if dialog.busy.is_none() {
                    dialog.active = row;
                    dialog.focus.focus(window, cx);
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    /// One line's choices: Native sets them in a segmented control's track, which wraps
    /// like the colorful themes' row when there are more than fit (Codex accounts).
    fn chips(&self, chips: Vec<AnyElement>, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        if ui_text::is_native() {
            return div()
                .flex()
                .child(
                    controls::segments(colors)
                        .flex_shrink_1()
                        .flex_wrap()
                        .children(chips),
                )
                .into_any_element();
        }
        div()
            .flex()
            .flex_wrap()
            .gap(ui_text::space(6.0))
            .children(chips)
            .into_any_element()
    }

    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        row: Row,
        primary: bool,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let disabled = self.busy.is_some() && primary;
        if ui_text::is_native() {
            // Capsules: the hand off filled with the primary color, Cancel grey, the
            // keyboard's button ringed, and no return key after the label.
            let kind = match (primary, disabled) {
                (_, true) => Button::Disabled,
                (true, false) => Button::Primary,
                (false, false) => Button::Secondary,
            };
            return controls::button(
                div()
                    .id(id)
                    .py(ui_text::space(5.0))
                    .child(label.trim_end_matches(['↵', ' '])),
                kind,
                colors,
            )
            .when(focused, |button| button.border_color(rgb(colors.focus)))
            .when(!disabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(kind.hover(colors))))
            })
            .on_click(cx.listener(move |dialog, _, _, cx| dialog.press(row, cx)))
            .into_any_element();
        }
        div()
            .id(id)
            .px(ui_text::space(12.0))
            .py(ui_text::space(8.0))
            .border_1()
            .border_color(rgb(if focused {
                colors.focus
            } else if primary {
                colors.cyan
            } else {
                colors.panel
            }))
            .bg(rgb(if primary {
                colors.panel_active
            } else {
                colors.panel
            }))
            .text_color(rgb(if disabled || !primary {
                colors.muted
            } else {
                colors.cyan
            }))
            .child(ui_text::cased(label))
            .on_click(cx.listener(move |dialog, _, _, cx| dialog.press(row, cx)))
            .into_any_element()
    }
}

/// The source is looked up when the work starts, off the UI thread; the request is shaped
/// before, with this in its place.
fn placeholder_source() -> handoff::Source {
    handoff::Source::Chat(crate::chat::model::ChatInfo {
        id: String::new(),
        provider: crate::chat::model::Provider::Codex,
        project_id: None,
        worktree_id: None,
        cwd: PathBuf::new(),
        title: String::new(),
        created_at_unix: 0,
        provider_thread_id: None,
        model: None,
        effort: None,
        approval_mode: Default::default(),
        codex_account_id: None,
        orchestrator: None,
        fast: false,
        state: Default::default(),
    })
}

impl Render for HandoffDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let focused = self.focus.is_focused(window);
        let entity = cx.entity();
        let busy = self.busy.is_some();

        let target = {
            let chips = [Kind::Shell, Kind::Chat]
                .into_iter()
                .enumerate()
                .map(|(index, kind)| {
                    self.chip(
                        ("handoff-target", index),
                        kind_label(kind).to_owned(),
                        self.choices.kind == kind,
                        true,
                        cx,
                        move |dialog, cx| {
                            dialog.pick(Row::Target, |dialog| dialog.choices.set_kind(kind), cx)
                        },
                    )
                })
                .collect();
            self.line(Row::Target, "Open it as a", self.chips(chips, cx), cx)
        };
        let agent = {
            let chips = [HarnessKind::Codex, HarnessKind::Claude, HarnessKind::Grok]
                .into_iter()
                .enumerate()
                .map(|(index, harness)| {
                    let enabled = self.choices.agents().contains(&harness);
                    self.chip(
                        ("handoff-agent", index),
                        agent_label(harness).to_owned(),
                        self.choices.provider == harness,
                        enabled,
                        cx,
                        move |dialog, cx| {
                            dialog.pick(Row::Agent, |dialog| dialog.choices.provider = harness, cx)
                        },
                    )
                })
                .collect();
            self.line(Row::Agent, "Agent", self.chips(chips, cx), cx)
        };
        let model = {
            let active = self.active == Row::Model && focused && !busy;
            let suggestions: Vec<AnyElement> = self
                .choices
                .suggestions()
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let name = (*name).to_owned();
                    let selected = self.model.text == name;
                    self.chip(
                        ("handoff-model", index),
                        name.clone(),
                        selected,
                        true,
                        cx,
                        move |dialog, cx| {
                            dialog.pick(
                                Row::Model,
                                |dialog| dialog.model = Input::new(name.clone()),
                                cx,
                            )
                        },
                    )
                })
                .collect();
            let content = div()
                .flex()
                .flex_col()
                .gap(ui_text::space(6.0))
                .child(input_content(
                    &self.model,
                    active,
                    "The agent's own default",
                    &self.focus,
                    entity.clone(),
                    colors,
                ))
                .children((!suggestions.is_empty()).then(|| self.chips(suggestions, cx)))
                .into_any_element();
            self.line(Row::Model, "Model", content, cx)
        };
        let account = (self.choices.provider == HarnessKind::Codex).then(|| {
            let chips = self
                .accounts
                .iter()
                .enumerate()
                .map(|(index, (label, _))| {
                    self.chip(
                        ("handoff-account", index),
                        label.clone(),
                        self.account == index,
                        true,
                        cx,
                        move |dialog, cx| {
                            dialog.pick(Row::Account, |dialog| dialog.account = index, cx)
                        },
                    )
                })
                .collect();
            self.line(Row::Account, "Codex account", self.chips(chips, cx), cx)
        });
        let context = self.choices.askable.then(|| {
            let chips = [Passing::Transcript, Passing::Summary]
                .into_iter()
                .enumerate()
                .map(|(index, context)| {
                    self.chip(
                        ("handoff-context", index),
                        context_label(context).to_owned(),
                        self.choices.context == context,
                        true,
                        cx,
                        move |dialog, cx| {
                            dialog.pick(Row::Context, |dialog| dialog.choices.context = context, cx)
                        },
                    )
                })
                .collect();
            self.line(Row::Context, "What it reads", self.chips(chips, cx), cx)
        });
        let note = {
            let active = self.active == Row::Note && focused && !busy;
            self.line(
                Row::Note,
                "Note (optional)",
                input_content(
                    &self.note,
                    active,
                    "Anything the new agent should know",
                    &self.focus,
                    entity.clone(),
                    colors,
                ),
                cx,
            )
        };
        let description = match self.choices.context {
            Passing::Summary if self.choices.askable => {
                "The agent is asked to write a summary first, which can take a few minutes; if it does not, the transcript is used."
            }
            _ => {
                "The new agent reads this conversation, written down, and carries on. The original stays as it is."
            }
        };
        div()
            .id("handoff-dialog")
            .occlude()
            .track_focus(&self.focus)
            .key_context("HandoffDialog")
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .w(ui_text::space(480.0))
            .max_w_full()
            .max_h(gpui::relative(0.9))
            .overflow_y_scroll()
            .p(ui_text::space(18.0))
            .flex()
            .flex_col()
            .gap(ui_text::space(12.0))
            .font_family(ui_text::ui_family())
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.magenta))
            // Native: a sheet with rounded corners, a hairline and a shadow, on the panels'
            // grey so its segmented controls stand out as they do in Settings.
            .map(|dialog| {
                controls::native(dialog, |dialog| {
                    dialog
                        .rounded(controls::radius(12.0))
                        .border_color(rgb(colors.divider))
                        .shadow_lg()
                })
            })
            .text_color(rgb(colors.text))
            .text_size(ui_text::text(11.0))
            .child(if ui_text::is_native() {
                // A title in semibold, as a panel's; the ellipsis belongs to the menu item.
                div()
                    .text_size(ui_text::text(controls::TITLE_TEXT))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Hand off")
            } else {
                div()
                    .text_color(rgb(colors.cyan))
                    .child(ui_text::cased("Hand off…").to_string())
            })
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .text_size(ui_text::text(10.0))
                    .child(format!("From {}. {description}", self.source.label)),
            )
            .child(target)
            .child(agent)
            .child(model)
            .children(account)
            .children(context)
            .child(note)
            .children(self.busy.clone().map(|busy| {
                div()
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(3.0))
                    .child(div().text_color(rgb(colors.cyan)).child(busy))
                    .child(
                        div()
                            .text_size(ui_text::text(10.0))
                            .text_color(rgb(colors.muted))
                            .child(
                                "Esc hides this and lets it finish. If the agent asks to write the summary, answer in its tab.",
                            ),
                    )
            }))
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_color(rgb(colors.gold)).child(error)),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(ui_text::space(10.0))
                    .child(self.button(
                        "handoff-cancel",
                        if busy { "Hide" } else { "Cancel" },
                        Row::Cancel,
                        false,
                        focused && self.active == Row::Cancel,
                        cx,
                    ))
                    .child(self.button(
                        "handoff-start",
                        "Hand off  ↵",
                        Row::Start,
                        true,
                        focused && self.active == Row::Start,
                        cx,
                    )),
            )
    }
}

impl_input_handler!(HandoffDialog);

#[cfg(test)]
mod tests {
    use super::*;

    fn choices(kind: Kind, provider: HarnessKind, askable: bool) -> Choices {
        Choices {
            kind,
            provider,
            context: Passing::Transcript,
            askable,
        }
    }

    #[test]
    fn the_lines_follow_the_choices() {
        use Row::*;
        let rows = |kind, provider, askable| choices(kind, provider, askable).rows();
        // A Codex has accounts to pick; an agent that can be asked has a way to be read.
        assert_eq!(
            rows(Kind::Chat, HarnessKind::Codex, true),
            [Target, Agent, Model, Account, Context, Note, Cancel, Start]
        );
        assert_eq!(
            rows(Kind::Shell, HarnessKind::Claude, false),
            [Target, Agent, Model, Note, Cancel, Start]
        );
    }

    #[test]
    fn a_chat_cannot_be_grok_and_a_terminal_can() {
        let mut picked = choices(Kind::Shell, HarnessKind::Grok, true);
        assert_eq!(picked.agents().len(), 3);
        picked.set_kind(Kind::Chat);
        assert_eq!(picked.provider, HarnessKind::Codex, "Grok has no chat");
        assert_eq!(picked.agents(), [HarnessKind::Codex, HarnessKind::Claude]);
        picked.provider = HarnessKind::Claude;
        picked.set_kind(Kind::Shell);
        assert_eq!(picked.provider, HarnessKind::Claude, "a fine choice stays");
    }

    #[test]
    fn models_are_free_text_with_a_few_suggestions() {
        let suggestions = |provider| choices(Kind::Chat, provider, true).suggestions();
        assert_eq!(
            suggestions(HarnessKind::Claude),
            ["opus", "sonnet", "haiku"]
        );
        assert!(!suggestions(HarnessKind::Codex).is_empty());
        assert!(suggestions(HarnessKind::Grok).is_empty());
    }

    #[test]
    fn accounts_are_the_projects_own_then_the_saved_ones() {
        let home = std::env::temp_dir().join(format!("riwork-dialog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        // Nothing saved: the project's own, or the system's.
        assert_eq!(
            account_choices(&home),
            [
                ("Project default".to_owned(), None),
                (
                    "System default".to_owned(),
                    Some("system-default".to_owned())
                ),
            ]
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
