//! Project metadata lives in a project tab; virtual folders never move files.

use std::{collections::BTreeMap, ops::Range};

use crate::text_input::{self, EnterBehavior, InputEvent, InputState};
use gpui::{
    AnyElement, App, ClipboardItem, Context, ElementInputHandler, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, HighlightStyle, IntoElement, KeyDownEvent, MouseButton, Render,
    StyledText, Window, canvas, div, prelude::*, rgb,
};
use gpui::{Focusable, Subscription};

use crate::{
    behavior_controls as behavior,
    chat::model::Provider,
    delegation_choice::{self, DelegationPolicy, Route, RouteChoice, Work},
    icons::{self, ActionGlyph, Icon},
    memo_editor::{MemoEditor, MemoEditorEvent},
    orchestrator_choice::{self, ModelChoice},
    settings::{CodexAccountsState, OrchestratorRuns, Settings, refresh_codex_accounts},
    store::{Project, ProjectCodexAccount, ProjectFolder, ProjectOrchestrator, State, Store},
    theme::{self, Palette},
    tooltip::{self, Look},
    ui_text, utf16_to_byte,
};

pub enum ProjectSettingsEvent {
    Saved(Project),
    FolderChanged,
}

pub enum FolderEditorEvent {
    Saved(ProjectFolder),
    Cancelled,
}

/// Dispatch through Base Root so its active Dialog focus trap owns traversal.
/// Explicit dispatch also works when the focused Input unbinds the Root key binding.
pub(crate) fn modal_tab(shift: bool, window: &mut Window, cx: &mut App) {
    let action = cx
        .build_action(if shift { "root::TabPrev" } else { "root::Tab" }, None)
        .expect("Base Root actions are initialized with text_input");
    window.dispatch_action(action, cx);
}

pub(crate) fn close_modal(
    dialog: &gpui_kit::base::DialogHandle,
    scope: &FocusHandle,
    previous: &Option<FocusHandle>,
    window: &mut Window,
    cx: &mut App,
) {
    let restore = scope.contains_focused(window, cx);
    dialog.close(window, cx);
    if restore && let Some(previous) = previous {
        previous.focus(window, cx);
    }
}

/// Existing Native toolbar presentation over the foundation's unstyled Base button.
/// This helper supplies only RiWork visuals; Base owns focus and activation.
pub(crate) fn kit_toolbar_button(
    id: impl Into<gpui::ElementId>,
    symbol: &'static str,
    name: impl Into<gpui::SharedString>,
    enabled: bool,
    colors: Palette,
) -> behavior::Button {
    let name = name.into();
    let rest = if enabled {
        colors.muted
    } else {
        theme::mix(colors.muted, colors.panel, 0.45)
    };
    behavior::button_content(id, name.clone(), icons::symbol(symbol, 11.0, None))
        .disabled(!enabled)
        .flex_none()
        .size(ui_text::space(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_color(rgb(rest))
        .when(enabled, |button| {
            button.hover(move |style| style.bg(rgb(colors.divider)).text_color(rgb(colors.text)))
        })
        .border_1()
        .border_color(gpui::transparent_black())
        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
        .child(tooltip::anchor(name, Look::Control))
}

#[derive(Default)]
pub(crate) struct Input {
    pub(crate) text: String,
    pub(crate) selection: Range<usize>,
    pub(crate) reversed: bool,
    pub(crate) marked: Option<Range<usize>>,
}

impl Input {
    pub(crate) fn new(text: String) -> Self {
        let end = text.len();
        Self {
            text,
            selection: end..end,
            ..Default::default()
        }
    }

    pub(crate) fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    pub(crate) fn selected_text(&self) -> Option<&str> {
        (!self.selection.is_empty()).then(|| &self.text[self.selection.clone()])
    }

    pub(crate) fn replace(&mut self, range: Option<Range<usize>>, text: &str) {
        self.splice(range, &single_line(text));
    }

    /// Like `replace`, for an input of several lines (the chat composer): line breaks
    /// stay, every kind of them as `\n`.
    pub(crate) fn replace_lines(&mut self, range: Option<Range<usize>>, text: &str) {
        self.splice(range, &line_breaks_as_newlines(text));
    }

    fn splice(&mut self, range: Option<Range<usize>>, text: &str) {
        let range = range
            .map(|range| {
                utf16_to_byte(&self.text, range.start)..utf16_to_byte(&self.text, range.end)
            })
            .or(self.marked.take())
            .unwrap_or_else(|| self.selection.clone());
        self.text.replace_range(range.clone(), text);
        let end = range.start + text.len();
        self.selection = end..end;
        self.reversed = false;
    }

    pub(crate) fn key(&mut self, event: &KeyDownEvent, cx: &mut App) -> bool {
        let platform = event.keystroke.modifiers.platform;
        match event.keystroke.key.as_str() {
            "a" if platform => {
                self.selection = 0..self.text.len();
                self.reversed = false;
            }
            "c" | "x" if platform => {
                // With nothing selected, copying "" would wipe the clipboard.
                if let Some(selected) = self.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(selected.to_owned()));
                    if event.keystroke.key == "x" {
                        self.replace(None, "");
                    }
                }
            }
            "v" if platform => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.replace(None, &text);
                }
            }
            "backspace" | "delete" => {
                if self.selection.is_empty() {
                    let cursor = self.cursor();
                    self.selection = if event.keystroke.key == "backspace" {
                        self.text[..cursor]
                            .char_indices()
                            .next_back()
                            .map(|(offset, _)| offset)
                            .unwrap_or(0)..cursor
                    } else {
                        cursor
                            ..self.text[cursor..]
                                .chars()
                                .next()
                                .map(|ch| cursor + ch.len_utf8())
                                .unwrap_or(cursor)
                    };
                }
                self.replace(None, "");
            }
            "left" | "right" | "home" | "end" => {
                let cursor = self.cursor();
                let offset = match event.keystroke.key.as_str() {
                    "home" => 0,
                    "end" => self.text.len(),
                    "left" if platform => 0,
                    "right" if platform => self.text.len(),
                    "left" if !event.keystroke.modifiers.shift && !self.selection.is_empty() => {
                        self.selection.start
                    }
                    "right" if !event.keystroke.modifiers.shift && !self.selection.is_empty() => {
                        self.selection.end
                    }
                    "left" => self.text[..cursor]
                        .char_indices()
                        .next_back()
                        .map(|(offset, _)| offset)
                        .unwrap_or(0),
                    _ => self.text[cursor..]
                        .chars()
                        .next()
                        .map(|ch| cursor + ch.len_utf8())
                        .unwrap_or(cursor),
                };
                if event.keystroke.modifiers.shift {
                    let anchor = if self.reversed {
                        self.selection.end
                    } else {
                        self.selection.start
                    };
                    self.selection = anchor.min(offset)..anchor.max(offset);
                    self.reversed = offset < anchor;
                } else {
                    self.selection = offset..offset;
                    self.reversed = false;
                }
                self.marked = None;
            }
            _ => return false,
        }
        true
    }
}

/// Pasted or composed text for an input of several lines: `\r\n`, `\r` and the Unicode
/// line separators become `\n`.
fn line_breaks_as_newlines(text: &str) -> String {
    let mut lines = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines.push('\n');
            }
            '\u{85}' | '\u{2028}' | '\u{2029}' => lines.push('\n'),
            _ => lines.push(c),
        }
    }
    lines
}

/// These inputs are single-line. A line break between pasted lines becomes one
/// space so words do not fuse; a copied line's trailing break is dropped.
pub(crate) fn single_line(text: &str) -> String {
    let is_break = |c: char| matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}');
    let mut line = String::with_capacity(text.len());
    let mut pending_break = false;
    for c in text.trim_matches(is_break).chars() {
        if is_break(c) {
            if !pending_break {
                line.push(' ');
            }
            pending_break = true;
        } else {
            pending_break = false;
            line.push(c);
        }
    }
    line
}

pub(crate) fn input_content<T: EntityInputHandler>(
    input: &Input,
    active: bool,
    placeholder: &str,
    focus: &FocusHandle,
    entity: Entity<T>,
    colors: Palette,
) -> AnyElement {
    let handler = active.then(|| {
        let focus = focus.clone();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                window.handle_input(&focus, ElementInputHandler::new(bounds, entity.clone()), cx);
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    });
    let mut text = if input.text.is_empty() {
        placeholder.to_owned()
    } else {
        input.text.clone()
    };
    let mut highlights = Vec::new();
    if active {
        if input.selection.is_empty() {
            let cursor = input.cursor();
            text.insert(cursor, '▌');
            highlights.push((
                cursor..cursor + '▌'.len_utf8(),
                HighlightStyle {
                    color: Some(rgb(colors.cyan).into()),
                    ..Default::default()
                },
            ));
        } else {
            highlights.push((
                input.selection.clone(),
                HighlightStyle {
                    color: Some(rgb(colors.cyan).into()),
                    background_color: Some(rgb(colors.divider).into()),
                    ..Default::default()
                },
            ));
        }
    }
    div()
        .relative()
        .cursor_text()
        .h(ui_text::space(34.0))
        .px(ui_text::space(10.0))
        .flex()
        .items_center()
        .min_w_0()
        .bg(rgb(colors.bg))
        .border_1()
        .border_color(rgb(if active { colors.cyan } else { colors.divider }))
        // Native: a rounded field with a hairline, ringed while it has the keyboard.
        .map(|field| {
            crate::controls::native(field, |field| {
                crate::controls::field(field, colors)
                    .h(ui_text::space(28.0))
                    .when(active, |field| field.border_color(rgb(colors.focus)))
            })
        })
        .text_color(rgb(if input.text.is_empty() {
            colors.muted
        } else {
            colors.text
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(StyledText::new(text).with_highlights(highlights)),
        )
        .children(handler)
        .into_any_element()
}

fn section(label: &str, colors: Palette) -> AnyElement {
    div()
        .pb(ui_text::space(8.0))
        .border_b_1()
        .border_color(rgb(colors.divider))
        .text_size(ui_text::text(10.0))
        .text_color(rgb(colors.cyan))
        .map(|heading| {
            // Native: a navigation list's section heading, without the number or the rule.
            crate::controls::native(heading, |heading| {
                heading
                    .border_b_0()
                    .pb_0()
                    .text_size(ui_text::text(crate::controls::META_TEXT))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(colors.muted))
            })
        })
        .child(if ui_text::is_native() {
            ui_text::sentence_case(
                label
                    .trim_start_matches(|c: char| c.is_ascii_digit())
                    .trim(),
            )
        } else {
            label.to_owned()
        })
        .into_any_element()
}

fn folder_choices(state: &State) -> (Vec<ProjectFolder>, BTreeMap<String, String>) {
    let paths = state
        .project_folders
        .iter()
        .map(|folder| (folder.id.clone(), state.project_folder_path(&folder.id)))
        .collect::<BTreeMap<_, _>>();
    let mut folders = state.project_folders.clone();
    folders.sort_by(|left, right| {
        paths[&left.id]
            .to_lowercase()
            .cmp(&paths[&right.id].to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    (folders, paths)
}

const SAVE_PROMPT: &str = "Save changes  ↵ / CMD+S";

/// What the footer says about saving. Name and folder edits wait for an
/// explicit save; the Codex account is stored the moment it is picked, so
/// choosing one says nothing about edits still pending.
#[derive(Default)]
struct SaveStatus {
    pending: bool,
    notice: Option<&'static str>,
}

impl SaveStatus {
    fn edited(&mut self) {
        self.pending = true;
        self.notice = None;
    }

    fn details_saved(&mut self) {
        self.pending = false;
        self.notice = Some("Settings saved");
    }

    fn account_saved(&mut self) {
        if !self.pending {
            self.notice = Some("Codex account saved");
        }
    }

    fn orchestrator_saved(&mut self) {
        if !self.pending {
            self.notice = Some("Orchestrator choice saved");
        }
    }

    fn delegation_saved(&mut self) {
        if !self.pending {
            self.notice = Some("Delegation policy saved");
        }
    }

    fn message(&self) -> &'static str {
        match self.notice {
            Some(notice) if !self.pending => notice,
            _ => SAVE_PROMPT,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Folder(usize),
    FolderName,
    AddFolder,
    Account(usize),
    AccountRefresh,
    /// The orchestrator's mode chips (`ORCHESTRATOR_MODES`), then for a chat its
    /// provider and model chips and "Other model ID".
    Orchestrator(usize),
    OrchestratorProvider(usize),
    OrchestratorModel(usize),
    OrchestratorModelInput,
    /// The delegation rows: "Spread work" (`SPREAD_CHOICES`), the worker shell Inherit
    /// chip and stepper (`PARALLEL_CONTROLS`), and each work's Inherit and route chips
    /// (`ProjectSettingsPanel::route_choices`).
    DelegationSpread(usize),
    DelegationParallel(usize),
    DelegationRoute(Work, usize),
    Save,
    /// The project's memo (07 MEMO), last in the keyboard order as it is on screen.
    Memo,
}

/// The mode chips of 04 ORCHESTRATOR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OrchestratorPick {
    Inherit,
    Automatic,
    Terminal,
    Chat,
}

const ORCHESTRATOR_MODES: [(OrchestratorPick, &str, &str); 4] = [
    (
        OrchestratorPick::Inherit,
        "Inherit app default",
        "project-orchestrator-inherit",
    ),
    (
        OrchestratorPick::Automatic,
        "Automatic",
        "project-orchestrator-automatic",
    ),
    (
        OrchestratorPick::Terminal,
        "Terminal",
        "project-orchestrator-terminal",
    ),
    (OrchestratorPick::Chat, "Chat", "project-orchestrator-chat"),
];

/// The "Spread work" chips of 05 DELEGATION: Inherit, then a choice.
const SPREAD_CHOICES: [(Option<bool>, &str); 3] = [
    (None, "project-delegation-spread-inherit"),
    (Some(true), "project-delegation-spread-on"),
    (Some(false), "project-delegation-spread-off"),
];

/// The worker shell controls of 05 DELEGATION: the Inherit chip, then − and +.
const PARALLEL_CONTROLS: [&str; 3] = [
    "project-delegation-parallel-inherit",
    "project-delegation-parallel-fewer",
    "project-delegation-parallel-more",
];

/// What the worker shell controls do: follow Settings, or one fewer or more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParallelPick {
    Inherit,
    Step { more: bool },
}

const PARALLEL_PICKS: [ParallelPick; 3] = [
    ParallelPick::Inherit,
    ParallelPick::Step { more: false },
    ParallelPick::Step { more: true },
];

/// A route chip's id in 05 DELEGATION: `project-delegation-design-claude`, `…-inherit`.
fn route_chip_id(work: Work, word: &str) -> String {
    format!("project-delegation-{}-{word}", work.key())
}

/// The last word of a route chip's id; `None` is Inherit.
fn route_word(choice: &Option<RouteChoice>) -> &'static str {
    choice.as_ref().map_or("inherit", |choice| choice.id)
}

/// Which mode chip a saved choice shows; none for one this build cannot read.
fn orchestrator_pick(choice: &ProjectOrchestrator) -> Option<OrchestratorPick> {
    match choice {
        ProjectOrchestrator::Inherit => Some(OrchestratorPick::Inherit),
        ProjectOrchestrator::Choose(OrchestratorRuns::Automatic) => {
            Some(OrchestratorPick::Automatic)
        }
        ProjectOrchestrator::Choose(OrchestratorRuns::Terminal) => Some(OrchestratorPick::Terminal),
        ProjectOrchestrator::Choose(OrchestratorRuns::Chat { .. }) => Some(OrchestratorPick::Chat),
        ProjectOrchestrator::Unreadable(_) => None,
    }
}

pub struct ProjectSettingsPanel {
    store: Store,
    project: Project,
    /// The project's memo, which saves itself (`memo_editor`).
    memo: Entity<MemoEditor>,
    /// The panel's scroll, so that the memo, the last section, can be brought into view.
    scroll: gpui::ScrollHandle,
    name: Input,
    name_state: Entity<InputState>,
    name_touched: bool,
    _input_subscriptions: Vec<Subscription>,
    folder_name: Input,
    folder_name_state: Entity<InputState>,
    /// "Other model ID" of a chat orchestrator.
    orchestrator_model_state: Entity<InputState>,
    orchestrator_error: Option<String>,
    /// Why a delegation choice was not saved.
    delegation_error: Option<String>,
    folders: Vec<ProjectFolder>,
    folder_paths: BTreeMap<String, String>,
    folder_id: Option<String>,
    active: Field,
    focus: FocusHandle,
    control_focus: BTreeMap<String, FocusHandle>,
    error: Option<String>,
    status: SaveStatus,
}

impl EventEmitter<ProjectSettingsEvent> for ProjectSettingsPanel {}

impl ProjectSettingsPanel {
    pub fn new(
        store: Store,
        project: Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe_global::<theme::Appearance>(|_, cx| cx.notify())
            .detach();
        cx.observe_global::<CodexAccountsState>(|_, cx| cx.notify())
            .detach();
        // The create-folder button swaps between words and a glyph with this setting.
        cx.observe_global::<Settings>(|_, cx| cx.notify()).detach();
        if cx.global::<CodexAccountsState>().snapshot.is_none() {
            refresh_codex_accounts(cx);
        }
        let snapshot = store.snapshot();
        let error = snapshot.as_ref().err().cloned();
        let memo = snapshot
            .as_ref()
            .map(|state| state.project_memo(&project.id))
            .unwrap_or_default();
        let memo = {
            let (store, project_id) = (store.clone(), project.id.clone());
            cx.new(|cx| MemoEditor::new(store, project_id, memo, window, cx))
        };
        let (folders, folder_paths) = snapshot
            .map(|state| folder_choices(&state))
            .unwrap_or_default();
        let name_state = text_input::single_line(project.name.clone(), "Project name", window, cx);
        let folder_name_state = text_input::single_line(
            "",
            if project.folder_id.is_some() {
                "New subfolder"
            } else {
                "New virtual folder"
            },
            window,
            cx,
        );
        let orchestrator_model_state = text_input::single_line("", "Other model ID", window, cx);
        let mut subscriptions =
            vec![
                cx.subscribe_in(&memo, window, |form, _, MemoEditorEvent::Focused, _, cx| {
                    form.active = Field::Memo;
                    cx.notify();
                }),
            ];
        for (field, state) in [
            (Field::Name, &name_state),
            (Field::FolderName, &folder_name_state),
            (Field::OrchestratorModelInput, &orchestrator_model_state),
        ] {
            subscriptions.push(cx.subscribe_in(
                state,
                window,
                move |form, state, event, window, cx| match event {
                    // Typing a model is not an unsaved project edit: Return saves it.
                    InputEvent::Change if field == Field::OrchestratorModelInput => {}
                    InputEvent::Change => {
                        let value = state.read(cx).value().to_string();
                        match field {
                            Field::Name => {
                                form.name.text = value;
                                form.name_touched = true;
                            }
                            _ => form.folder_name.text = value,
                        }
                        form.edited(window, cx);
                    }
                    InputEvent::Focus => {
                        form.active = field;
                        cx.notify();
                    }
                    _ if text_input::is_submit(event, EnterBehavior::Submit) => match field {
                        Field::Name => form.save(window, cx),
                        Field::OrchestratorModelInput => form.use_typed_model(window, cx),
                        _ => form.create_folder(window, cx),
                    },
                    _ => {}
                },
            ));
        }
        Self {
            name_state,
            name_touched: false,
            folder_name_state,
            orchestrator_model_state,
            orchestrator_error: None,
            delegation_error: None,
            _input_subscriptions: subscriptions,
            name: Input::new(project.name.clone()),
            folder_id: project.folder_id.clone(),
            project,
            memo,
            scroll: gpui::ScrollHandle::new(),
            store,
            folders,
            folder_paths,
            folder_name: Input::default(),
            active: Field::Name,
            focus: cx.focus_handle(),
            control_focus: BTreeMap::new(),
            error,
            status: SaveStatus::default(),
        }
    }

    fn visible_controls(&self, cx: &App) -> Vec<(String, Field)> {
        let mut controls = vec![("project-folder-unfiled".into(), Field::Folder(0))];
        controls.extend(self.folders.iter().enumerate().map(|(index, folder)| {
            (
                format!("project-folder-{}", folder.id),
                Field::Folder(index + 1),
            )
        }));
        controls.extend(self.account_choices(cx).into_iter().enumerate().map(
            |(index, (choice, _, _))| {
                let id = match choice {
                    ProjectCodexAccount::Inherit => "project-codex-inherit".into(),
                    ProjectCodexAccount::SystemDefault => "project-codex-system".into(),
                    ProjectCodexAccount::Saved(id) => format!("project-codex-saved-{id}"),
                };
                (id, Field::Account(index))
            },
        ));
        controls.extend(
            ORCHESTRATOR_MODES
                .iter()
                .enumerate()
                .map(|(index, (_, _, id))| ((*id).to_owned(), Field::Orchestrator(index))),
        );
        if let Some((provider, model)) = self.orchestrator_chat() {
            controls.extend(orchestrator_choice::PROVIDERS.iter().enumerate().map(
                |(index, provider)| {
                    (
                        provider_chip_id(*provider),
                        Field::OrchestratorProvider(index),
                    )
                },
            ));
            controls.extend(
                orchestrator_choice::model_choices(provider, model.as_deref())
                    .iter()
                    .enumerate()
                    .map(|(index, choice)| {
                        (model_chip_id(choice), Field::OrchestratorModel(index))
                    }),
            );
        }
        controls.extend(
            SPREAD_CHOICES
                .iter()
                .enumerate()
                .map(|(index, (_, id))| ((*id).to_owned(), Field::DelegationSpread(index))),
        );
        controls.extend(
            PARALLEL_CONTROLS
                .iter()
                .enumerate()
                .map(|(index, id)| ((*id).to_owned(), Field::DelegationParallel(index))),
        );
        for work in Work::ALL {
            controls.extend(
                self.route_choices(work)
                    .iter()
                    .enumerate()
                    .map(|(index, choice)| {
                        (
                            route_chip_id(work, route_word(choice)),
                            Field::DelegationRoute(work, index),
                        )
                    }),
            );
        }
        controls.extend([
            ("project-settings-create-folder".into(), Field::AddFolder),
            ("project-codex-refresh".into(), Field::AccountRefresh),
            ("project-settings-save".into(), Field::Save),
        ]);
        controls
    }

    /// The project's chat orchestrator choice, if it chose a chat.
    fn orchestrator_chat(&self) -> Option<(Provider, Option<String>)> {
        match &self.project.orchestrator {
            ProjectOrchestrator::Choose(OrchestratorRuns::Chat { provider, model }) => {
                Some((*provider, model.clone()))
            }
            _ => None,
        }
    }

    /// Saves how this project's orchestrator runs when it is next created, at once, as
    /// the Codex account is saved.
    fn select_orchestrator(&mut self, choice: ProjectOrchestrator, cx: &mut Context<Self>) {
        match self
            .store
            .set_project_orchestrator(&self.project.id, choice)
        {
            Ok(project) => {
                self.project = project.clone();
                self.orchestrator_error = None;
                self.error = None;
                self.status.orchestrator_saved();
                cx.emit(ProjectSettingsEvent::Saved(project));
            }
            Err(error) => self.orchestrator_error = Some(error),
        }
        cx.notify();
    }

    /// Saves a change to this project's delegation policy at once, as the orchestrator
    /// choice is saved. Its orchestrator is told when it is next created or loads its skill.
    fn change_delegation(
        &mut self,
        change: impl FnOnce(&mut DelegationPolicy),
        cx: &mut Context<Self>,
    ) {
        match self
            .store
            .set_project_delegation_policy(&self.project.id, change)
        {
            Ok(project) => {
                self.project = project.clone();
                self.delegation_error = None;
                self.error = None;
                self.status.delegation_saved();
                cx.emit(ProjectSettingsEvent::Saved(project));
            }
            Err(error) => self.delegation_error = Some(error),
        }
        cx.notify();
    }

    fn pick_spread(&mut self, spread: Option<bool>, cx: &mut Context<Self>) {
        self.change_delegation(move |policy| policy.set_spread_work(spread), cx);
    }

    /// Inherit, or one worker shell fewer or more than the project's soft target or,
    /// while it inherits, than Settings'.
    fn pick_parallel(&mut self, pick: ParallelPick, cx: &mut Context<Self>) {
        let shells = match pick {
            ParallelPick::Inherit => None,
            ParallelPick::Step { more } => {
                let Some(next) = delegation_choice::step_shells(self.parallel_shells(cx), more)
                else {
                    return;
                };
                Some(next)
            }
        };
        self.change_delegation(move |policy| policy.set_parallel_shells(shells), cx);
    }

    /// The soft target the stepper shows: the project's own, or Settings' it inherits.
    fn parallel_shells(&self, cx: &App) -> u8 {
        self.project
            .delegation_policy
            .parallel_shells
            .unwrap_or_else(|| {
                delegation_choice::settings_policy(cx.global::<Settings>()).parallel_shells
            })
    }

    fn pick_route(&mut self, work: Work, route: Option<Route>, cx: &mut Context<Self>) {
        self.change_delegation(move |policy| policy.set_route(work, route), cx);
    }

    /// A work's chips: Inherit (`None`), then the offered routes and the project's saved
    /// route if it is another.
    fn route_choices(&self, work: Work) -> Vec<Option<RouteChoice>> {
        std::iter::once(None)
            .chain(
                delegation_choice::route_choices(self.project.delegation_policy.route(work))
                    .into_iter()
                    .map(Some),
            )
            .collect()
    }

    /// A mode chip: a chat chosen now starts as the chat Settings keep for chat mode.
    fn pick_orchestrator(&mut self, pick: OrchestratorPick, cx: &mut Context<Self>) {
        let choice = match pick {
            OrchestratorPick::Inherit => ProjectOrchestrator::Inherit,
            OrchestratorPick::Automatic => ProjectOrchestrator::Choose(OrchestratorRuns::Automatic),
            OrchestratorPick::Terminal => ProjectOrchestrator::Choose(OrchestratorRuns::Terminal),
            OrchestratorPick::Chat if self.orchestrator_chat().is_some() => return,
            OrchestratorPick::Chat => {
                let settings = cx.global::<Settings>();
                ProjectOrchestrator::Choose(OrchestratorRuns::Chat {
                    provider: settings.orchestrator_chat_provider,
                    model: settings.orchestrator_chat_model.clone(),
                })
            }
        };
        self.select_orchestrator(choice, cx);
    }

    /// A provider chip: another provider starts on its own default model.
    fn pick_orchestrator_provider(&mut self, provider: Provider, cx: &mut Context<Self>) {
        let Some((current, model)) = self.orchestrator_chat() else {
            return;
        };
        let model = if current == provider { model } else { None };
        self.select_orchestrator(
            ProjectOrchestrator::Choose(OrchestratorRuns::Chat { provider, model }),
            cx,
        );
    }

    fn pick_orchestrator_model(&mut self, model: Option<String>, cx: &mut Context<Self>) {
        let Some((provider, _)) = self.orchestrator_chat() else {
            return;
        };
        self.select_orchestrator(
            ProjectOrchestrator::Choose(OrchestratorRuns::Chat { provider, model }),
            cx,
        );
    }

    /// Return in "Other model ID".
    fn use_typed_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match orchestrator_choice::typed_model(&self.orchestrator_model_state.read(cx).value()) {
            Ok(model) => {
                self.pick_orchestrator_model(Some(model), cx);
                if self.orchestrator_error.is_none() {
                    crate::form_input::set_value(
                        &self.orchestrator_model_state,
                        String::new(),
                        window,
                        cx,
                    );
                }
            }
            Err(error) => {
                self.orchestrator_error = Some(error);
                cx.notify();
            }
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active {
            Field::Memo => self.memo.update(cx, |memo, cx| memo.focus(window, cx)),
            Field::Name => self.name_state.read(cx).focus_handle(cx).focus(window, cx),
            Field::OrchestratorModelInput => self
                .orchestrator_model_state
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx),
            Field::FolderName => self
                .folder_name_state
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx),
            _ => {
                if let Some((id, _)) = self
                    .visible_controls(cx)
                    .into_iter()
                    .find(|(_, field)| *field == self.active)
                    && let Some(focus) = self.control_focus.get(&id)
                {
                    focus.focus(window, cx);
                } else {
                    self.focus.focus(window, cx);
                }
            }
        }
    }

    /// The memo in view, with the keys: a chat's `/memo` **Edit**.
    pub fn reveal_memo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active = Field::Memo;
        self.focus(window, cx);
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Refresh choices without replacing the user's partially edited project name, and the
    /// memo when it was saved elsewhere and has no unsaved edits here.
    pub fn refresh_folders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.store.snapshot() {
            Ok(state) => {
                self.memo
                    .update(cx, |memo, cx| memo.refresh(&state, window, cx));
                let focused_folder = match self.active {
                    Field::Folder(index) => index
                        .checked_sub(1)
                        .and_then(|index| self.folders.get(index))
                        .map(|folder| folder.id.clone()),
                    _ => None,
                };
                let (folders, folder_paths) = folder_choices(&state);
                if let Some(project) = state
                    .projects
                    .into_iter()
                    .find(|project| project.id == self.project.id)
                {
                    if project.name != self.project.name {
                        crate::form_input::refresh_unedited(
                            &self.name_state,
                            &self.project.name,
                            project.name.clone(),
                            self.name_touched,
                            window,
                            cx,
                        );
                        self.name.text = self.name_state.read(cx).value().to_string();
                    }
                    if self.folder_id == self.project.folder_id {
                        self.folder_id = project.folder_id.clone();
                    }
                    self.project = project;
                }
                self.folders = folders;
                self.folder_paths = folder_paths;
                if self
                    .folder_id
                    .as_ref()
                    .is_some_and(|id| !self.folders.iter().any(|folder| &folder.id == id))
                {
                    self.folder_id = None;
                }
                if let Some(id) = focused_folder {
                    self.active = Field::Folder(
                        self.folders
                            .iter()
                            .position(|folder| folder.id == id)
                            .map(|index| index + 1)
                            .unwrap_or(0),
                    );
                } else if matches!(self.active, Field::Folder(index) if index > self.folders.len())
                {
                    self.active = Field::Folder(0);
                }
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn edited(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::form_input::placeholder(
            &self.folder_name_state,
            if self.folder_id.is_some() {
                "New subfolder"
            } else {
                "New virtual folder"
            },
            window,
            cx,
        );
        self.status.edited();
        self.error = None;
        cx.notify();
    }

    fn accepts_input(&self) -> bool {
        matches!(
            self.active,
            Field::Name | Field::FolderName | Field::OrchestratorModelInput
        )
    }

    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active {
            // Return in the memo is a new line, never an activation.
            Field::Memo => {}
            Field::Name | Field::Save => self.save(window, cx),
            Field::FolderName | Field::AddFolder => self.create_folder(window, cx),
            Field::Folder(index) => {
                self.folder_id = index
                    .checked_sub(1)
                    .and_then(|index| self.folders.get(index))
                    .map(|folder| folder.id.clone());
                self.edited(window, cx);
            }
            Field::Account(index) => {
                if let Some((choice, _, available)) = self.account_choices(cx).get(index) {
                    if *available {
                        self.select_account(choice.clone(), cx);
                    }
                }
            }
            Field::AccountRefresh => refresh_codex_accounts(cx),
            Field::Orchestrator(index) => {
                if let Some((pick, _, _)) = ORCHESTRATOR_MODES.get(index) {
                    self.pick_orchestrator(*pick, cx);
                }
            }
            Field::OrchestratorProvider(index) => {
                if let Some(provider) = orchestrator_choice::PROVIDERS.get(index) {
                    self.pick_orchestrator_provider(*provider, cx);
                }
            }
            Field::OrchestratorModel(index) => {
                if let Some((provider, model)) = self.orchestrator_chat()
                    && let Some(choice) =
                        orchestrator_choice::model_choices(provider, model.as_deref()).get(index)
                {
                    self.pick_orchestrator_model(choice.model.clone(), cx);
                }
            }
            Field::OrchestratorModelInput => self.use_typed_model(window, cx),
            Field::DelegationSpread(index) => {
                if let Some((spread, _)) = SPREAD_CHOICES.get(index) {
                    self.pick_spread(*spread, cx);
                }
            }
            Field::DelegationParallel(index) => {
                if let Some(pick) = PARALLEL_PICKS.get(index) {
                    self.pick_parallel(*pick, cx);
                }
            }
            Field::DelegationRoute(work, index) => {
                if let Some(choice) = self.route_choices(work).get(index) {
                    let route = choice.as_ref().map(|choice| choice.route.clone());
                    self.pick_route(work, route, cx);
                }
            }
        }
    }

    fn account_choices(&self, cx: &App) -> Vec<(ProjectCodexAccount, String, bool)> {
        let mut choices = vec![
            (
                ProjectCodexAccount::Inherit,
                "Inherit app default".to_owned(),
                true,
            ),
            (
                ProjectCodexAccount::SystemDefault,
                "System default".to_owned(),
                true,
            ),
        ];
        if let Some(snapshot) = &cx.global::<CodexAccountsState>().snapshot {
            choices.extend(
                snapshot
                    .accounts
                    .iter()
                    .filter(|account| !account.is_system_default)
                    .map(|account| {
                        (
                            ProjectCodexAccount::Saved(account.id.clone()),
                            account.label.clone(),
                            account.available,
                        )
                    }),
            );
        }
        if let ProjectCodexAccount::Saved(id) = &self.project.codex_account {
            if !choices
                .iter()
                .any(|(choice, _, _)| choice == &self.project.codex_account)
            {
                choices.push((
                    ProjectCodexAccount::Saved(id.clone()),
                    format!(
                        "Saved account unavailable · {}",
                        id.chars().take(8).collect::<String>()
                    ),
                    false,
                ));
            }
        }
        choices
    }

    fn select_account(&mut self, choice: ProjectCodexAccount, cx: &mut Context<Self>) {
        match self
            .store
            .set_project_codex_account(&self.project.id, choice)
        {
            Ok(project) => {
                self.project = project.clone();
                self.error = None;
                self.status.account_saved();
                cx.emit(ProjectSettingsEvent::Saved(project));
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.name.text = self.name_state.read(cx).value().to_string();
        match self.store.update_project_metadata(
            &self.project.id,
            self.name.text.trim(),
            self.folder_id.as_deref(),
        ) {
            Ok(project) => {
                self.name = Input::new(project.name.clone());
                crate::form_input::set_value(&self.name_state, project.name.clone(), window, cx);
                self.name_touched = false;
                self.project = project.clone();
                self.error = None;
                self.status.details_saved();
                cx.emit(ProjectSettingsEvent::Saved(project));
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn create_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.folder_name.text = self.folder_name_state.read(cx).value().to_string();
        match self
            .store
            .create_project_folder_in(self.folder_name.text.trim(), self.folder_id.as_deref())
        {
            Ok(folder) => {
                self.folder_id = Some(folder.id.clone());
                self.folder_name = Input::default();
                crate::form_input::set_value(&self.folder_name_state, String::new(), window, cx);
                self.folders.push(folder);
                self.active = Field::Name;
                self.error = None;
                self.status.edited();
                self.refresh_folders(window, cx);
                self.focus(window, cx);
                cx.emit(ProjectSettingsEvent::FolderChanged);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, field)) = self.visible_controls(cx).into_iter().find(|(id, _)| {
            self.control_focus
                .get(id)
                .is_some_and(|focus| focus.is_focused(window))
        }) {
            self.active = field;
        }
        let input = match self.active {
            Field::Name => Some(&self.name_state),
            Field::FolderName => Some(&self.folder_name_state),
            Field::OrchestratorModelInput => Some(&self.orchestrator_model_state),
            _ => None,
        };
        if matches!(event.keystroke.key.as_str(), "escape" | "tab")
            && (input.is_some_and(|state| crate::form_input::is_composing(state, window, cx))
                || self.active == Field::Memo
                    && self
                        .memo
                        .update(cx, |memo, cx| memo.is_composing(window, cx)))
        {
            return;
        }
        let handled = match event.keystroke.key.as_str() {
            "left" | "up" if matches!(self.active, Field::Folder(_)) => {
                if let Field::Folder(index) = self.active {
                    self.active = Field::Folder(index.checked_sub(1).unwrap_or(self.folders.len()));
                }
                true
            }
            "right" | "down" if matches!(self.active, Field::Folder(_)) => {
                if let Field::Folder(index) = self.active {
                    self.active = Field::Folder((index + 1) % (self.folders.len() + 1));
                }
                true
            }
            "left" | "up" | "right" | "down" if matches!(self.active, Field::Account(_)) => {
                if let Field::Account(index) = self.active {
                    let choices = self.account_choices(cx);
                    let available = choices
                        .iter()
                        .enumerate()
                        .filter(|(_, (_, _, available))| *available)
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>();
                    if !available.is_empty() {
                        let at = available
                            .iter()
                            .position(|candidate| *candidate == index)
                            .unwrap_or(0);
                        let next = if matches!(event.keystroke.key.as_str(), "left" | "up") {
                            (at + available.len() - 1) % available.len()
                        } else {
                            (at + 1) % available.len()
                        };
                        self.active = Field::Account(available[next]);
                    }
                }
                true
            }
            "left" | "up" | "right" | "down"
                if matches!(
                    self.active,
                    Field::Orchestrator(_)
                        | Field::OrchestratorProvider(_)
                        | Field::OrchestratorModel(_)
                        | Field::DelegationSpread(_)
                        | Field::DelegationParallel(_)
                        | Field::DelegationRoute(..)
                ) =>
            {
                let back = matches!(event.keystroke.key.as_str(), "left" | "up");
                let step = |index: usize, count: usize| {
                    if back {
                        (index + count - 1) % count
                    } else {
                        (index + 1) % count
                    }
                };
                self.active = match self.active {
                    Field::Orchestrator(index) => {
                        Field::Orchestrator(step(index, ORCHESTRATOR_MODES.len()))
                    }
                    Field::OrchestratorProvider(index) => Field::OrchestratorProvider(step(
                        index,
                        orchestrator_choice::PROVIDERS.len(),
                    )),
                    Field::OrchestratorModel(index) => {
                        let count = self
                            .orchestrator_chat()
                            .map(|(provider, model)| {
                                orchestrator_choice::model_choices(provider, model.as_deref()).len()
                            })
                            .unwrap_or(1);
                        Field::OrchestratorModel(step(index, count))
                    }
                    Field::DelegationSpread(index) => {
                        Field::DelegationSpread(step(index, SPREAD_CHOICES.len()))
                    }
                    Field::DelegationParallel(index) => {
                        Field::DelegationParallel(step(index, PARALLEL_CONTROLS.len()))
                    }
                    Field::DelegationRoute(work, index) => {
                        Field::DelegationRoute(work, step(index, self.route_choices(work).len()))
                    }
                    other => other,
                };
                true
            }
            "s" if event.keystroke.modifiers.platform => {
                self.memo.update(cx, |memo, cx| memo.save_now(cx));
                self.save(window, cx);
                true
            }
            "tab" => {
                let mut order = vec![Field::Name];
                order.extend((0..=self.folders.len()).map(Field::Folder));
                order.extend([Field::FolderName, Field::AddFolder]);
                order.extend(
                    self.account_choices(cx)
                        .iter()
                        .enumerate()
                        .filter(|(_, (_, _, available))| *available)
                        .map(|(index, _)| Field::Account(index)),
                );
                if !cx.global::<CodexAccountsState>().pending {
                    order.push(Field::AccountRefresh);
                }
                order.extend((0..ORCHESTRATOR_MODES.len()).map(Field::Orchestrator));
                if let Some((provider, model)) = self.orchestrator_chat() {
                    order.extend(
                        (0..orchestrator_choice::PROVIDERS.len()).map(Field::OrchestratorProvider),
                    );
                    order.extend(
                        (0..orchestrator_choice::model_choices(provider, model.as_deref()).len())
                            .map(Field::OrchestratorModel),
                    );
                    order.push(Field::OrchestratorModelInput);
                }
                order.extend((0..SPREAD_CHOICES.len()).map(Field::DelegationSpread));
                order.extend((0..PARALLEL_CONTROLS.len()).map(Field::DelegationParallel));
                for work in Work::ALL {
                    order.extend(
                        (0..self.route_choices(work).len())
                            .map(|index| Field::DelegationRoute(work, index)),
                    );
                }
                order.extend([Field::Save, Field::Memo]);
                let index = order
                    .iter()
                    .position(|field| *field == self.active)
                    .unwrap_or(0);
                let next = if event.keystroke.modifiers.shift {
                    (index + order.len() - 1) % order.len()
                } else {
                    (index + 1) % order.len()
                };
                self.active = order[next];
                self.focus(window, cx);
                true
            }
            _ => false,
        };
        if handled {
            if matches!(
                event.keystroke.key.as_str(),
                "left" | "right" | "up" | "down"
            ) {
                self.focus(window, cx);
            }
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn field(&self, field: Field, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let (state, id) = match field {
            Field::Name => (&self.name_state, "project-settings-name"),
            Field::FolderName => (&self.folder_name_state, "project-settings-new-folder"),
            Field::OrchestratorModelInput => (
                &self.orchestrator_model_state,
                "project-orchestrator-model-input",
            ),
            _ => unreachable!("Only text fields render an input"),
        };
        crate::form_input::frame(id, state, false, window, cx).into_any_element()
    }
}

/// A chip's id in 04 ORCHESTRATOR.
fn provider_chip_id(provider: Provider) -> String {
    format!(
        "project-orchestrator-provider-{}",
        orchestrator_choice::provider_word(provider)
    )
}

fn model_chip_id(choice: &ModelChoice) -> String {
    format!("project-orchestrator-model-{}", choice.id)
}

impl ProjectSettingsPanel {
    /// One chip of 04 ORCHESTRATOR or 05 DELEGATION, drawn as the account chips are.
    #[allow(clippy::too_many_arguments)]
    fn orchestrator_chip(
        &self,
        id: String,
        label: String,
        selected: bool,
        field: Field,
        focused: bool,
        colors: Palette,
        on_pick: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ring = focused && self.active == field;
        let chip = behavior::radio_content(
            id.clone(),
            label.clone(),
            div()
                .when(ui_text::is_native(), |mark| mark.hidden())
                .child(if selected { "◉" } else { "○" }),
            selected,
        )
        .track_focus(&self.control_focus[&id])
        .max_w(ui_text::space(300.0))
        .min_w_0()
        .px(ui_text::space(10.0))
        .py(ui_text::space(7.0))
        .flex()
        .items_center()
        .gap(ui_text::space(7.0))
        .border_1()
        .border_color(rgb(if ring {
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
        .map(|chip| native_choice(chip, selected, ring, colors))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(label),
        )
        .on_change({
            let listener = cx.listener(move |form, _, _window, cx| {
                form.active = field;
                on_pick(form, cx);
            });
            move |_, event, window, cx| listener(event, window, cx)
        });
        crate::form_input::control_element(id, chip)
    }

    /// The rows of 04 ORCHESTRATOR: the mode chips and, for a chat, its provider, its
    /// model and "Other model ID"; then why a saved value is not shown, if one is not.
    fn orchestrator_section(
        &self,
        focused: bool,
        colors: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let group = |id: &str, label: &str| {
            gpui_kit::base::RadioGroup::new(id.to_owned())
                .axis(gpui::Axis::Horizontal)
                .aria_label(label.to_owned())
                .flex()
                .flex_wrap()
                .gap(ui_text::space(6.0))
        };
        let caption = |text: String, color: u32| {
            div()
                .text_size(ui_text::text(10.0))
                .text_color(rgb(color))
                .child(text)
        };
        let picked = orchestrator_pick(&self.project.orchestrator);
        let inherited = orchestrator_choice::short(&cx.global::<Settings>().orchestrator_runs());
        let mut modes = group("project-orchestrator-choices", "Orchestrator");
        for (index, (pick, label, id)) in ORCHESTRATOR_MODES.into_iter().enumerate() {
            let label = if pick == OrchestratorPick::Inherit {
                format!("{label} · {inherited}")
            } else {
                label.to_owned()
            };
            modes = modes.child(self.orchestrator_chip(
                id.to_owned(),
                label,
                picked == Some(pick),
                Field::Orchestrator(index),
                focused,
                colors,
                move |form, cx| form.pick_orchestrator(pick, cx),
                cx,
            ));
        }
        let mut rows = vec![modes.into_any_element()];
        if picked == Some(OrchestratorPick::Automatic) {
            rows.push(
                caption(orchestrator_choice::automatic_note(), colors.muted).into_any_element(),
            );
        }
        if let Some((provider, model)) = self.orchestrator_chat() {
            let mut providers = group("project-orchestrator-providers", "Chat provider");
            for (index, choice) in orchestrator_choice::PROVIDERS.into_iter().enumerate() {
                providers = providers.child(self.orchestrator_chip(
                    provider_chip_id(choice),
                    crate::session_catalog::provider_label(choice).to_owned(),
                    choice == provider,
                    Field::OrchestratorProvider(index),
                    focused,
                    colors,
                    move |form, cx| form.pick_orchestrator_provider(choice, cx),
                    cx,
                ));
            }
            let mut models = group("project-orchestrator-models", "Chat model");
            for (index, choice) in orchestrator_choice::model_choices(provider, model.as_deref())
                .into_iter()
                .enumerate()
            {
                let selected = choice.model == model;
                let value = choice.model.clone();
                models = models.child(self.orchestrator_chip(
                    model_chip_id(&choice),
                    choice.label,
                    selected,
                    Field::OrchestratorModel(index),
                    focused,
                    colors,
                    move |form, cx| form.pick_orchestrator_model(value.clone(), cx),
                    cx,
                ));
            }
            rows.push(caption("Chat provider".to_owned(), colors.muted).into_any_element());
            rows.push(providers.into_any_element());
            rows.push(caption("Chat model".to_owned(), colors.muted).into_any_element());
            rows.push(models.into_any_element());
            rows.push(
                div()
                    .flex()
                    .flex_wrap()
                    .min_w_0()
                    .items_center()
                    .gap(ui_text::space(8.0))
                    .child(div().w(ui_text::space(260.0)).child(self.field(
                        Field::OrchestratorModelInput,
                        window,
                        cx,
                    )))
                    .child(caption(
                        "Another model ID: type it and press Return.".to_owned(),
                        colors.muted,
                    ))
                    .into_any_element(),
            );
        }
        if let Some(note) = self.project.orchestrator.unreadable_note() {
            rows.push(caption(note, colors.gold).into_any_element());
        }
        if let Some(error) = &self.orchestrator_error {
            rows.push(caption(error.clone(), colors.gold).into_any_element());
        }
        rows
    }

    /// The rows of 05 DELEGATION, each led by an Inherit chip that names Settings' value:
    /// "Spread work", the worker shell stepper, and the design and workhorse routes; then
    /// why a saved value is not shown, and why a choice was not saved.
    fn delegation_section(
        &self,
        focused: bool,
        colors: Palette,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let group = |id: &str, label: &str| {
            gpui_kit::base::RadioGroup::new(id.to_owned())
                .axis(gpui::Axis::Horizontal)
                .aria_label(label.to_owned())
                .flex()
                .flex_wrap()
                .gap(ui_text::space(6.0))
        };
        let caption = |text: String, color: u32| {
            div()
                .text_size(ui_text::text(10.0))
                .text_color(rgb(color))
                .child(text)
        };
        // A row: its title, its controls, and one sentence on what it decides.
        let row = |title: &str, controls: AnyElement, about: &str| {
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .gap(ui_text::space(6.0))
                .child(
                    div()
                        .text_size(ui_text::text(11.0))
                        .text_color(rgb(colors.text))
                        .child(title.to_owned()),
                )
                .child(controls)
                .child(caption(about.to_owned(), colors.muted))
                .into_any_element()
        };
        let policy = self.project.delegation_policy.clone();
        let inherited = delegation_choice::settings_policy(cx.global::<Settings>());

        let mut spread = group(
            "project-delegation-spread-choices",
            delegation_choice::SPREAD_TITLE,
        );
        for (index, (choice, id)) in SPREAD_CHOICES.into_iter().enumerate() {
            let label = match choice {
                None => format!(
                    "Inherit · {}",
                    delegation_choice::on_off(inherited.spread_work)
                ),
                Some(on) => delegation_choice::on_off(on).to_owned(),
            };
            spread = spread.child(self.orchestrator_chip(
                id.to_owned(),
                label,
                policy.spread_work == choice,
                Field::DelegationSpread(index),
                focused,
                colors,
                move |form, cx| form.pick_spread(choice, cx),
                cx,
            ));
        }
        let mut rows = vec![row(
            delegation_choice::SPREAD_TITLE,
            spread.into_any_element(),
            "Orchestrators fan independent tasks out to several worker shells at once instead of working through them one by one.",
        )];

        let shells = self.parallel_shells(cx);
        let parallel = div()
            .flex()
            .flex_wrap()
            .min_w_0()
            .items_center()
            .gap(ui_text::space(6.0))
            .child(
                group(
                    "project-delegation-parallel-choices",
                    delegation_choice::PARALLEL_TITLE,
                )
                .child(self.orchestrator_chip(
                    PARALLEL_CONTROLS[0].to_owned(),
                    format!("Inherit · {}", inherited.parallel_shells),
                    policy.parallel_shells.is_none(),
                    Field::DelegationParallel(0),
                    focused,
                    colors,
                    |form, cx| form.pick_parallel(ParallelPick::Inherit, cx),
                    cx,
                )),
            )
            .child(self.parallel_step(false, shells, focused, colors, cx))
            .child(
                div()
                    .flex_none()
                    .min_w(ui_text::space(28.0))
                    .text_center()
                    // Settings' value while inheriting, as the Inherit chip says.
                    .text_color(rgb(if policy.parallel_shells.is_some() {
                        colors.cyan
                    } else {
                        colors.muted
                    }))
                    .child(shells.to_string()),
            )
            .child(self.parallel_step(true, shells, focused, colors, cx));
        rows.push(row(
            delegation_choice::PARALLEL_TITLE,
            parallel.into_any_element(),
            "A soft target for how many worker shells this project's orchestrator runs at once; RiWork starts none by itself.",
        ));

        for work in Work::ALL {
            let mut routes = group(
                &format!("project-delegation-{}-choices", work.key()),
                work.label(),
            );
            for (index, choice) in self.route_choices(work).into_iter().enumerate() {
                let id = route_chip_id(work, route_word(&choice));
                let label = match &choice {
                    None => format!("Inherit · {}", inherited.route(work).label()),
                    Some(choice) => choice.label.clone(),
                };
                let route = choice.map(|choice| choice.route);
                routes = routes.child(self.orchestrator_chip(
                    id,
                    label,
                    policy.route(work) == route.as_ref(),
                    Field::DelegationRoute(work, index),
                    focused,
                    colors,
                    move |form, cx| form.pick_route(work, route.clone(), cx),
                    cx,
                ));
            }
            rows.push(row(
                work.label(),
                routes.into_any_element(),
                delegation_choice::route_description(work),
            ));
        }
        if let Some(note) = policy.unreadable_note("This project's") {
            rows.push(caption(note, colors.gold).into_any_element());
        }
        if let Some(error) = &self.delegation_error {
            rows.push(caption(error.clone(), colors.gold).into_any_element());
        }
        rows
    }

    /// The worker shell stepper's − or +, disabled past either end of `PARALLEL_SHELLS`.
    fn parallel_step(
        &self,
        more: bool,
        shells: u8,
        focused: bool,
        colors: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let index = if more { 2 } else { 1 };
        let (id, pick) = (PARALLEL_CONTROLS[index], PARALLEL_PICKS[index]);
        let (name, symbol) = if more {
            ("More worker shells", "+")
        } else {
            ("Fewer worker shells", "−")
        };
        let field = Field::DelegationParallel(index);
        let active = delegation_choice::step_shells(shells, more).is_some();
        let ring = focused && self.active == field;
        let button = behavior::button_content(id, name, symbol)
            .disabled(!active)
            .track_focus(&self.control_focus[id])
            .focus_visible(move |style| style.border_color(rgb(colors.focus)))
            .flex_none()
            .min_w(ui_text::space(28.0))
            .px(ui_text::space(8.0))
            .py(ui_text::space(7.0))
            .text_center()
            .border_1()
            .border_color(rgb(if ring {
                colors.focus
            } else if active {
                colors.cyan
            } else {
                colors.divider
            }))
            .bg(rgb(colors.bg))
            .text_color(rgb(if active { colors.cyan } else { colors.muted }))
            .map(|button| {
                crate::controls::native(button, |button| {
                    let kind = if active {
                        crate::controls::Button::Secondary
                    } else {
                        crate::controls::Button::Disabled
                    };
                    crate::controls::button(button, kind, colors)
                        .py(ui_text::space(4.0))
                        .hover(move |style| style.bg(rgb(kind.hover(colors))))
                        .when(ring, |button| button.border_color(rgb(colors.focus)))
                })
            })
            .on_click(cx.listener(move |form, _, _window, cx| {
                form.active = field;
                form.pick_parallel(pick, cx);
            }));
        crate::form_input::control_element(id, button)
    }
}

impl Render for ProjectSettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let membership = self.visible_controls(cx);
        self.control_focus
            .retain(|id, _| membership.iter().any(|(visible, _)| visible == id));
        for (id, _) in membership {
            self.control_focus
                .entry(id)
                .or_insert_with(|| cx.focus_handle());
        }
        let dirty = self.name_state.read(cx).value().trim() != self.project.name
            || self.folder_id != self.project.folder_id;
        let focused = self.focus.contains_focused(window, cx);
        let mut folders = gpui_kit::base::RadioGroup::new("project-folder-choices")
            .axis(gpui::Axis::Horizontal)
            .aria_label("Project folder")
            .flex()
            .flex_wrap()
            .gap(ui_text::space(6.0))
            .child(
                behavior::radio_content(
                    "project-folder-unfiled",
                    "Unfiled",
                    ui_text::cased("Unfiled"),
                    self.folder_id.is_none(),
                )
                .track_focus(&self.control_focus["project-folder-unfiled"])
                .px(ui_text::space(10.0))
                .py(ui_text::space(7.0))
                .border_1()
                .border_color(rgb(if focused && self.active == Field::Folder(0) {
                    colors.focus
                } else if self.folder_id.is_none() {
                    colors.cyan
                } else {
                    colors.divider
                }))
                .bg(rgb(if self.folder_id.is_none() {
                    colors.panel_active
                } else {
                    colors.bg
                }))
                .text_color(rgb(if self.folder_id.is_none() {
                    colors.cyan
                } else {
                    colors.muted
                }))
                .map(|chip| {
                    native_choice(
                        chip,
                        self.folder_id.is_none(),
                        focused && self.active == Field::Folder(0),
                        colors,
                    )
                })
                .on_change({
                    let listener = cx.listener(|form, _, window, cx| {
                        form.active = Field::Folder(0);
                        form.folder_id = None;
                        form.edited(window, cx);
                    });
                    move |_, event, window, cx| listener(event, window, cx)
                }),
            );
        for (index, folder) in self.folders.iter().enumerate() {
            let id = folder.id.clone();
            let selected = self.folder_id.as_deref() == Some(folder.id.as_str());
            folders = folders.child(
                behavior::radio_content(
                    format!("project-folder-{}", folder.id),
                    self.folder_paths
                        .get(&folder.id)
                        .cloned()
                        .unwrap_or_else(|| folder.name.clone()),
                    self.folder_paths
                        .get(&folder.id)
                        .cloned()
                        .unwrap_or_else(|| folder.name.clone()),
                    selected,
                )
                .track_focus(&self.control_focus[&format!("project-folder-{}", folder.id)])
                .focus_visible(move |style| style.border_color(rgb(colors.focus)))
                .px(ui_text::space(10.0))
                .py(ui_text::space(7.0))
                .border_1()
                .border_color(rgb(if focused && self.active == Field::Folder(index + 1) {
                    colors.focus
                } else if selected {
                    colors.magenta
                } else {
                    colors.divider
                }))
                .bg(rgb(if selected {
                    colors.panel_active
                } else {
                    colors.bg
                }))
                .text_color(rgb(if selected {
                    colors.magenta
                } else {
                    colors.text
                }))
                .map(|chip| {
                    native_choice(
                        chip,
                        selected,
                        focused && self.active == Field::Folder(index + 1),
                        colors,
                    )
                })
                .on_change({
                    let listener = cx.listener(move |form, _, window, cx| {
                        if !form.folders.iter().any(|folder| folder.id == id) {
                            return;
                        }
                        form.active = Field::Folder(index + 1);
                        form.folder_id = Some(id.clone());
                        form.edited(window, cx);
                    });
                    move |_, event, window, cx| listener(event, window, cx)
                }),
            );
        }
        let mut repositories = div().flex().flex_col().gap(ui_text::space(6.0));
        if self.project.repository_roots.is_empty() {
            repositories = repositories.child(
                div()
                    .text_color(rgb(colors.muted))
                    .child("No Git repositories detected"),
            );
        } else {
            for root in &self.project.repository_roots {
                repositories = repositories.child(
                    div()
                        .px(ui_text::space(10.0))
                        .py(ui_text::space(8.0))
                        .bg(rgb(colors.bg))
                        .border_l_1()
                        .border_color(rgb(colors.magenta))
                        .map(|row| native_path(row, colors))
                        .child(root.to_string_lossy().into_owned()),
                );
            }
        }
        let account_state = cx.global::<CodexAccountsState>().clone();
        let mut account_rows = gpui_kit::base::RadioGroup::new("project-account-choices")
            .axis(gpui::Axis::Horizontal)
            .aria_label("Codex account")
            .flex()
            .flex_wrap()
            .gap(ui_text::space(6.0));
        for (index, (choice, label, available)) in self.account_choices(cx).into_iter().enumerate()
        {
            let selected = choice == self.project.codex_account;
            let id = match &choice {
                ProjectCodexAccount::Inherit => "project-codex-inherit".to_owned(),
                ProjectCodexAccount::SystemDefault => "project-codex-system".to_owned(),
                ProjectCodexAccount::Saved(id) => format!("project-codex-saved-{id}"),
            };
            account_rows = account_rows.child(
                behavior::radio_content(
                    id.clone(),
                    label.clone(),
                    div()
                        .when(ui_text::is_native(), |mark| mark.hidden())
                        .child(if selected { "◉" } else { "○" }),
                    selected,
                )
                .track_focus(&self.control_focus[&id])
                .disabled(!available)
                .max_w(ui_text::space(300.0))
                .min_w_0()
                .px(ui_text::space(10.0))
                .py(ui_text::space(7.0))
                .flex()
                .items_center()
                .gap(ui_text::space(7.0))
                .border_1()
                .border_color(rgb(if focused && self.active == Field::Account(index) {
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
                .text_color(rgb(if !available {
                    colors.muted
                } else if selected {
                    colors.cyan
                } else {
                    colors.text
                }))
                .map(|chip| {
                    native_choice(
                        chip,
                        selected,
                        focused && self.active == Field::Account(index),
                        colors,
                    )
                    .when(ui_text::is_native() && !available, |chip| {
                        chip.text_color(rgb(colors.muted))
                    })
                })
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(label),
                )
                .on_change({
                    let listener = cx.listener(move |form, _, _window, cx| {
                        form.active = Field::Account(index);
                        if available
                            && form
                                .account_choices(cx)
                                .iter()
                                .any(|(current, _, available)| *current == choice && *available)
                        {
                            form.select_account(choice.clone(), cx);
                        }
                    });
                    move |_, event, window, cx| listener(event, window, cx)
                }),
            );
        }
        let native = ui_text::is_native();
        // A section: a ruled box in the colorful themes, a Native card otherwise.
        let section_box = |gap: f32| {
            div()
                .p(ui_text::space(14.0))
                .flex()
                .flex_col()
                .gap(ui_text::space(gap))
                .bg(rgb(colors.panel))
                .border_1()
                .border_color(rgb(colors.divider))
                .map(|card| {
                    crate::controls::native(card, |card| crate::controls::card(card, colors))
                })
        };
        let label = |text: &str| {
            div()
                .text_color(rgb(colors.muted))
                .text_size(ui_text::text(10.0))
                .child(ui_text::cased(text.to_owned()))
        };
        let memo = section_box(8.0)
            .child(section("07  MEMO", colors))
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .text_size(ui_text::text(10.0))
                    .child(
                        "Your own Markdown notes for this project, saved as you type and shared \
                         with RiWork on your phone. Type /memo in one of its chats to insert them.",
                    ),
            )
            .child(self.memo.clone());
        let identity = section_box(12.0)
            .child(section("01  IDENTITY", colors))
            .child(label("Project name"))
            .child(self.field(Field::Name, window, cx));
        let folder = section_box(12.0)
            .child(section("02  VIRTUAL FOLDER", colors))
            .child(folders)
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .text_size(ui_text::text(10.0))
                    .child("Group projects in the browser. Files stay in their current locations."),
            )
            .children(
                self.folder_id
                    .as_ref()
                    .and_then(|id| self.folder_paths.get(id))
                    .map(|path| {
                        div()
                            .text_color(rgb(if native { colors.muted } else { colors.magenta }))
                            .text_size(ui_text::text(10.0))
                            .child(if native {
                                format!("New subfolders go under {path}")
                            } else {
                                format!("NEW SUBFOLDER UNDER  /  {path}")
                            })
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .min_w_0()
                    .items_center()
                    .gap(ui_text::space(8.0))
                    .child(self.field(Field::FolderName, window, cx))
                    .child({
                        let subfolder = self.folder_id.is_some();
                        let ring = focused && self.active == Field::AddFolder;
                        let button = behavior::button_content(
                            "project-settings-create-folder",
                            if subfolder {
                                "Add subfolder"
                            } else {
                                "Add folder"
                            },
                            if native {
                                div()
                                    .child(if subfolder {
                                        "Add subfolder"
                                    } else {
                                        "Add folder"
                                    })
                                    .into_any_element()
                            } else if icons::labels_as_icons(cx) {
                                icons::icon(Icon::Action(ActionGlyph::NewFolder), colors.magenta)
                            } else {
                                div()
                                    .child(if subfolder { "+ SUBFOLDER" } else { "+ FOLDER" })
                                    .into_any_element()
                            },
                        )
                        .h(ui_text::space(34.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .border_1()
                        .border_color(rgb(if ring { colors.focus } else { colors.magenta }))
                        .text_color(rgb(colors.magenta));
                        // Icons keep the button's size, colour and focus ring; the tooltip names it.
                        let button = button
                            .track_focus(&self.control_focus["project-settings-create-folder"])
                            .focus_visible(move |style| style.border_color(rgb(colors.focus)));
                        let button = if native {
                            let kind = crate::controls::Button::Secondary;
                            crate::controls::button(button, kind, colors)
                                .h(ui_text::space(28.0))
                                .hover(move |style| style.bg(rgb(kind.hover(colors))))
                                .when(ring, |button| button.border_color(rgb(colors.focus)))
                        } else if icons::labels_as_icons(cx) {
                            button
                                .w(ui_text::space(34.0))
                                .justify_center()
                                .child(tooltip::anchor(
                                    if subfolder {
                                        "New subfolder"
                                    } else {
                                        "New folder"
                                    },
                                    Look::Control,
                                ))
                        } else {
                            button.px(ui_text::space(12.0))
                        };
                        button.on_click(cx.listener(|form, _, window, cx| {
                            form.active = Field::AddFolder;
                            form.create_folder(window, cx);
                        }))
                    }),
            );
        let account = section_box(10.0)
            .child(section("03  CODEX ACCOUNT", colors))
            .child(div().text_size(ui_text::text(10.0)).text_color(rgb(colors.muted))
                .child("New Codex sessions use this choice. Running sessions keep their account. Selection saves immediately."))
            .child(account_rows)
            .child(behavior::button_content("project-codex-refresh", "Refresh accounts", ui_text::cased(if account_state.pending { "Checking accounts…" } else { "Refresh accounts" })).disabled(account_state.pending).track_focus(&self.control_focus["project-codex-refresh"]).focus_visible(move |style| style.border_color(rgb(colors.focus)))
                .text_size(ui_text::text(10.0))
                .text_color(rgb(if focused && self.active == Field::AccountRefresh { colors.focus } else { colors.cyan }))
                .map(|button| crate::controls::native(button, |button| {
                    let kind = crate::controls::Button::Secondary;
                    crate::controls::button(button, kind, colors)
                        .self_start()
                        .py(ui_text::space(3.0))
                        .hover(move |style| style.bg(rgb(kind.hover(colors))))
                        .when(focused && self.active == Field::AccountRefresh, |button| button.border_color(rgb(colors.focus)))
                }))
                .on_click(cx.listener(|form, _, _window, cx| {
                    form.active = Field::AccountRefresh;
                    refresh_codex_accounts(cx);
                })))
            .children(account_state.snapshot.as_ref().and_then(|snapshot| snapshot.error.as_ref()).map(|error|
                div().text_size(ui_text::text(10.0)).text_color(rgb(colors.gold)).child(error.clone())))
            .children(matches!(self.project.codex_account, ProjectCodexAccount::Saved(_))
                .then_some(self.project.codex_account.clone())
                .filter(|choice| self.account_choices(cx).iter().any(|(row, _, available)| row == choice && !available))
                .map(|_| div().text_size(ui_text::text(10.0)).text_color(rgb(colors.gold))
                    .child("Selected account unavailable. Choose another before starting Codex.")));
        let orchestrator = self.orchestrator_section(focused, colors, window, cx);
        let orchestrator = section_box(10.0)
            .child(section("04  ORCHESTRATOR", colors))
            .child(
                div()
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    .child("How this project's orchestrator runs when it is next created. Inherit follows Settings > Agent sessions; the others apply to this project only. The orchestrator that runs keeps its mode, provider and model until it is closed. Selection saves immediately."),
            )
            .children(orchestrator);
        let delegation = self.delegation_section(focused, colors, cx);
        let delegation = section_box(12.0)
            .child(section("05  DELEGATION", colors))
            .child(
                div()
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    .child("This project's orchestrator follows Settings' delegation policy except where you choose here. It is told the result when it is next created or loads its skill."),
            )
            .children(delegation);
        let save_ring = focused && self.active == Field::Save;
        let save = div()
            .flex()
            .flex_wrap()
            .min_w_0()
            .items_center()
            .justify_between()
            .gap(ui_text::space(10.0))
            .when(native, |row| {
                row.px(ui_text::space(
                    crate::controls::PANEL_INSET - crate::controls::LIST_MARGIN,
                ))
            })
            .child(
                div()
                    .min_w_0()
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(if self.error.is_some() {
                        colors.gold
                    } else {
                        colors.muted
                    }))
                    .child(
                        self.error
                            .clone()
                            .unwrap_or_else(|| self.status.message().into()),
                    ),
            )
            .child(
                behavior::button_content(
                    "project-settings-save",
                    "Save project",
                    ui_text::cased("Save project"),
                )
                .track_focus(&self.control_focus["project-settings-save"])
                .focus_visible(move |style| style.border_color(rgb(colors.focus)))
                .flex_none()
                .px(ui_text::space(14.0))
                .py(ui_text::space(10.0))
                .bg(rgb(colors.panel_active))
                .border_1()
                .border_color(rgb(if save_ring { colors.focus } else { colors.cyan }))
                .text_color(rgb(colors.cyan))
                .map(|button| {
                    crate::controls::native(button, |button| {
                        let kind = if dirty {
                            crate::controls::Button::Primary
                        } else {
                            crate::controls::Button::Secondary
                        };
                        crate::controls::button(button, kind, colors)
                            .py(ui_text::space(4.0))
                            .hover(move |style| style.bg(rgb(kind.hover(colors))))
                            .when(save_ring, |button| button.border_color(rgb(colors.focus)))
                    })
                })
                .on_click(cx.listener(|form, _, window, cx| {
                    form.active = Field::Save;
                    form.save(window, cx);
                })),
            );
        let locations = section_box(12.0)
            .child(section("06  LOCATIONS", colors))
            .child(label("Project root"))
            .child(
                div()
                    .px(ui_text::space(10.0))
                    .py(ui_text::space(8.0))
                    .bg(rgb(colors.bg))
                    .border_l_1()
                    .border_color(rgb(colors.cyan))
                    .map(|row| native_path(row, colors))
                    .child(self.project.root.to_string_lossy().into_owned()),
            )
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .text_size(ui_text::text(10.0))
                    .child(if native {
                        format!("Repositories · {}", self.project.repository_roots.len())
                    } else {
                        format!(
                            "REPOSITORIES  /  {:02}",
                            self.project.repository_roots.len()
                        )
                    }),
            )
            .child(repositories)
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .text_size(ui_text::text(9.0))
                    .child(if native {
                        format!("Project ID {}", self.project.id)
                    } else {
                        format!("PROJECT ID  /  {}", self.project.id)
                    }),
            );
        if native {
            return crate::controls::panel(colors)
                .id("project-settings-panel")
                .track_focus(&self.focus)
                .key_context("ProjectSettings")
                .capture_key_down(cx.listener(Self::key_down))
                .child(crate::controls::panel_header(
                    crate::layouts::PanelKind::ProjectSettings.label(),
                    Some(if dirty {
                        format!("{} · Unsaved", self.project.name).into()
                    } else {
                        self.project.name.clone().into()
                    }),
                    [],
                    colors,
                ))
                .child(
                    div()
                        .id("project-settings-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .px(ui_text::space(crate::controls::LIST_MARGIN))
                        .pb(ui_text::space(crate::controls::PANEL_INSET))
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .max_w(ui_text::space(760.0))
                                .flex()
                                .flex_col()
                                .gap(ui_text::space(8.0))
                                .child(identity)
                                .child(folder)
                                .child(account)
                                .child(orchestrator)
                                .child(delegation)
                                .child(save)
                                .child(locations)
                                .child(memo),
                        ),
                )
                .into_any_element();
        }
        div()
            .id("project-settings-panel")
            .size_full()
            .min_w_0()
            .track_focus(&self.focus)
            .key_context("ProjectSettings")
            .capture_key_down(cx.listener(Self::key_down))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .bg(rgb(colors.bg))
            .text_color(rgb(colors.text))
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(11.0))
            .p(ui_text::space(20.0))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .max_w(ui_text::space(760.0))
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(18.0))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .min_w_0()
                            .justify_between()
                            .items_center()
                            .gap(ui_text::space(12.0))
                            .border_l_2()
                            .border_color(rgb(colors.cyan))
                            .pl(ui_text::space(12.0))
                            .py(ui_text::space(6.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(ui_text::space(5.0))
                                    .child(
                                        div()
                                            .text_color(rgb(colors.cyan))
                                            .text_size(ui_text::text(16.0))
                                            .child(ui_text::cased("Project settings")),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .text_color(rgb(colors.muted))
                                            .text_size(ui_text::text(10.0))
                                            .child(self.project.name.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .px(ui_text::space(8.0))
                                    .py(ui_text::space(4.0))
                                    .border_1()
                                    .border_color(rgb(colors.divider))
                                    .text_size(ui_text::text(9.0))
                                    .text_color(rgb(if dirty {
                                        colors.magenta
                                    } else {
                                        colors.muted
                                    }))
                                    .child(ui_text::cased(if dirty {
                                        "Unsaved"
                                    } else {
                                        "Local project"
                                    })),
                            ),
                    )
                    .child(identity)
                    .child(folder)
                    .child(account)
                    .child(orchestrator)
                    .child(delegation)
                    .child(save)
                    .child(locations)
                    .child(memo),
            )
            .into_any_element()
    }
}

/// Native's choice among a few options (a folder, an account): a capsule, filled with the
/// primary color when chosen. The colorful themes keep their outlined boxes.
fn native_choice<E: gpui::Styled + gpui::InteractiveElement + gpui::prelude::FluentBuilder>(
    chip: E,
    selected: bool,
    ring: bool,
    colors: Palette,
) -> E {
    crate::controls::native(chip, |chip| {
        let kind = if selected {
            crate::controls::Button::Primary
        } else {
            crate::controls::Button::Secondary
        };
        crate::controls::button(chip, kind, colors)
            .py(ui_text::space(4.0))
            .hover(move |style| style.bg(rgb(kind.hover(colors))))
            .when(ring, |chip| chip.border_color(rgb(colors.focus)))
    })
}

/// A path under Native: monospace on a quiet rounded fill, without the colored rule.
fn native_path(row: gpui::Div, colors: Palette) -> gpui::Div {
    crate::controls::native(row, |row| {
        row.border_l_0()
            .rounded(crate::controls::radius(crate::controls::FIELD_RADIUS))
            .bg(rgb(colors.panel))
            .font_family(ui_text::mono_family())
            .text_size(ui_text::text(10.0))
    })
}

pub struct FolderEditor {
    store: Store,
    folder: Option<ProjectFolder>,
    parent_id: Option<String>,
    parent_path: Option<String>,
    name: Input,
    name_state: Entity<InputState>,
    _input_subscriptions: Vec<Subscription>,
    focus: FocusHandle,
    dialog: gpui_kit::base::DialogHandle,
    return_focus: Option<FocusHandle>,
    button_focus: [FocusHandle; 2],
    active: usize,
    error: Option<String>,
}

impl EventEmitter<FolderEditorEvent> for FolderEditor {}

impl FolderEditor {
    pub fn new(
        store: Store,
        folder: Option<ProjectFolder>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_in(store, folder, None, window, cx)
    }

    pub fn new_in(
        store: Store,
        folder: Option<ProjectFolder>,
        parent_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let return_focus = window.focused(cx);
        cx.observe_global::<theme::Appearance>(|_, cx| cx.notify())
            .detach();
        let parent_id = match &folder {
            Some(folder) => folder.parent_id.clone(),
            None => parent_id,
        };
        let snapshot = store.snapshot();
        let error = snapshot.as_ref().err().cloned();
        let parent_path = parent_id.as_ref().map(|id| {
            snapshot
                .as_ref()
                .map(|state| state.project_folder_path(id))
                .unwrap_or_else(|_| id.clone())
        });
        let name_state = text_input::single_line(
            folder.as_ref().map(|f| f.name.clone()).unwrap_or_default(),
            "Folder name",
            window,
            cx,
        );
        let subscription =
            cx.subscribe_in(
                &name_state,
                window,
                |form, state, event, _, cx| match event {
                    InputEvent::Change => {
                        form.name.text = state.read(cx).value().to_string();
                        form.edited(cx);
                    }
                    InputEvent::Focus => {
                        form.active = 0;
                        cx.notify();
                    }
                    _ if text_input::is_submit(event, EnterBehavior::Submit) => form.submit(cx),
                    _ => {}
                },
            );
        Self {
            name_state,
            _input_subscriptions: vec![subscription],
            name: Input::new(
                folder
                    .as_ref()
                    .map(|folder| folder.name.clone())
                    .unwrap_or_default(),
            ),
            store,
            folder,
            parent_id,
            parent_path,
            focus: cx.focus_handle(),
            dialog: gpui_kit::base::DialogHandle::new(true),
            return_focus,
            button_focus: [cx.focus_handle(), cx.focus_handle()],
            active: 0,
            error,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active == 0 {
            self.name_state.read(cx).focus_handle(cx).focus(window, cx);
        } else {
            self.button_focus[self.active - 1].focus(window, cx);
        }
    }

    fn accepts_input(&self) -> bool {
        self.active == 0
    }
    fn edited(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        self.name.text = self.name_state.read(cx).value().to_string();
        let result = match &self.folder {
            Some(folder) => self
                .store
                .rename_project_folder(&folder.id, self.name.text.trim()),
            None => self
                .store
                .create_project_folder_in(self.name.text.trim(), self.parent_id.as_deref()),
        };
        match result {
            Ok(folder) => cx.emit(FolderEditorEvent::Saved(folder)),
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(button) = self
            .button_focus
            .iter()
            .position(|focus| focus.is_focused(window))
        {
            self.active = button + 1;
        }

        if self.active == 0
            && matches!(event.keystroke.key.as_str(), "escape" | "tab")
            && crate::form_input::is_composing(&self.name_state, window, cx)
        {
            return;
        }
        let handled = match event.keystroke.key.as_str() {
            "escape" => {
                crate::project_settings::close_modal(
                    &self.dialog,
                    &self.focus,
                    &self.return_focus,
                    window,
                    cx,
                );
                cx.emit(FolderEditorEvent::Cancelled);
                true
            }
            "tab" => {
                crate::project_settings::modal_tab(event.keystroke.modifiers.shift, window, cx);
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }
}

impl Render for FolderEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let panel = div()
            .id("virtual-folder-editor")
            // Pinned Base Dialog's focus-trap host omits its AX role.
            .role(gpui::Role::Dialog)
            .aria_label(if self.folder.is_some() {
                "Rename virtual folder"
            } else if self.parent_id.is_some() {
                "New subfolder"
            } else {
                "New virtual folder"
            })
            .occlude()
            .key_context("FolderEditor")
            .capture_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .w(ui_text::space(440.0))
            .max_w_full()
            .max_h(gpui::relative(0.9))
            .overflow_y_scroll()
            .p(ui_text::space(18.0))
            .flex()
            .flex_col()
            .gap(ui_text::space(14.0))
            .font_family(ui_text::ui_family())
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.magenta))
            .text_color(rgb(colors.text))
            .text_size(ui_text::text(11.0))
            .child(div().text_color(rgb(colors.cyan)).child(ui_text::cased(
                if self.folder.is_some() {
                    "Rename virtual folder"
                } else if self.parent_id.is_some() {
                    "New subfolder"
                } else {
                    "New virtual folder"
                },
            )))
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .text_size(ui_text::text(10.0))
                    .child("Group projects without moving their files."),
            )
            .children(self.parent_path.as_ref().map(|path| {
                div()
                    .text_color(rgb(colors.magenta))
                    .text_size(ui_text::text(10.0))
                    .child(format!("PARENT  /  {path}"))
            }))
            .child(
                div()
                    .id("virtual-folder-name")
                    .child(crate::form_input::frame(
                        "virtual-folder-input",
                        &self.name_state,
                        false,
                        window,
                        cx,
                    ))
                    .on_click(cx.listener(|form, _, window, cx| {
                        form.active = 0;
                        form.focus(window, cx);
                        cx.notify();
                    })),
            )
            .children(
                self.error
                    .as_ref()
                    .map(|error| div().text_color(rgb(colors.gold)).child(error.clone())),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(ui_text::space(10.0))
                    .child(
                        behavior::button_content(
                            "cancel-virtual-folder",
                            "Cancel",
                            ui_text::cased("Cancel"),
                        )
                        .track_focus(&self.button_focus[0])
                        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
                        .px(ui_text::space(12.0))
                        .py(ui_text::space(8.0))
                        .border_1()
                        .border_color(rgb(
                            if self.active == 1 && self.focus.contains_focused(window, cx) {
                                colors.focus
                            } else {
                                colors.panel
                            },
                        ))
                        .text_color(rgb(colors.muted))
                        .on_click(cx.listener(|form, _, window, cx| {
                            close_modal(&form.dialog, &form.focus, &form.return_focus, window, cx);
                            cx.emit(FolderEditorEvent::Cancelled);
                        })),
                    )
                    .child(
                        behavior::button_content(
                            "save-virtual-folder",
                            "Save folder",
                            "SAVE FOLDER  ↵",
                        )
                        .track_focus(&self.button_focus[1])
                        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
                        .px(ui_text::space(12.0))
                        .py(ui_text::space(8.0))
                        .bg(rgb(colors.panel_active))
                        .border_1()
                        .border_color(rgb(
                            if self.active == 2 && self.focus.contains_focused(window, cx) {
                                colors.focus
                            } else {
                                colors.cyan
                            },
                        ))
                        .text_color(rgb(colors.cyan))
                        .on_click(cx.listener(|form, _, _, cx| form.submit(cx))),
                    ),
            );
        gpui_kit::base::Dialog::new(cx)
            .handle(self.dialog.clone())
            .focus_handle(self.focus.clone())
            .close_on_escape(false)
            .close_on_backdrop_press(false)
            .on_ok(|_, _, _| false)
            .on_cancel(|_, _, _| false)
            .popup(panel)
    }
}

macro_rules! impl_input_handler {
    ($view:ty) => {
        impl EntityInputHandler for $view {
            fn text_for_range(
                &mut self,
                range: Range<usize>,
                actual: &mut Option<Range<usize>>,
                _: &mut Window,
                _: &mut Context<Self>,
            ) -> Option<String> {
                let input = self.input();
                let start = utf16_to_byte(&input.text, range.start);
                let end = utf16_to_byte(&input.text, range.end);
                *actual = Some(
                    input.text[..start].encode_utf16().count()
                        ..input.text[..end].encode_utf16().count(),
                );
                Some(input.text[start..end].to_owned())
            }
            fn selected_text_range(
                &mut self,
                _: bool,
                _: &mut Window,
                _: &mut Context<Self>,
            ) -> Option<UTF16Selection> {
                let input = self.input();
                Some(UTF16Selection {
                    range: input.text[..input.selection.start].encode_utf16().count()
                        ..input.text[..input.selection.end].encode_utf16().count(),
                    reversed: input.reversed,
                })
            }
            fn marked_text_range(
                &self,
                _: &mut Window,
                _: &mut Context<Self>,
            ) -> Option<Range<usize>> {
                let input = self.input();
                input.marked.as_ref().map(|range| {
                    input.text[..range.start].encode_utf16().count()
                        ..input.text[..range.end].encode_utf16().count()
                })
            }
            fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
                self.input_mut().marked = None;
            }
            fn replace_text_in_range(
                &mut self,
                range: Option<Range<usize>>,
                text: &str,
                _: &mut Window,
                cx: &mut Context<Self>,
            ) {
                self.input_mut().replace(range, text);
                self.edited(cx);
            }
            fn replace_and_mark_text_in_range(
                &mut self,
                range: Option<Range<usize>>,
                text: &str,
                _: Option<Range<usize>>,
                _: &mut Window,
                cx: &mut Context<Self>,
            ) {
                self.input_mut().replace(range, text);
                let input = self.input_mut();
                let length = text.replace(['\n', '\r'], "").len();
                let end = input.selection.end;
                input.marked = (length > 0).then_some(end - length..end);
                self.edited(cx);
            }
            fn bounds_for_range(
                &mut self,
                _: Range<usize>,
                bounds: Bounds<Pixels>,
                _: &mut Window,
                _: &mut Context<Self>,
            ) -> Option<Bounds<Pixels>> {
                Some(bounds)
            }
            fn character_index_for_point(
                &mut self,
                _: Point<Pixels>,
                _: &mut Window,
                _: &mut Context<Self>,
            ) -> Option<usize> {
                Some(self.input().text.encode_utf16().count())
            }
            fn text_length_utf16(
                &mut self,
                _: &mut Window,
                _: &mut Context<Self>,
            ) -> Option<usize> {
                Some(self.input().text.encode_utf16().count())
            }
            fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
                self.accepts_input()
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choosing_an_account_never_claims_pending_edits_were_saved() {
        let mut status = SaveStatus::default();
        assert_eq!(status.message(), SAVE_PROMPT);
        status.account_saved();
        assert_eq!(status.message(), "Codex account saved");

        // A name edit is unsaved until SAVE PROJECT, whatever happens to the account.
        status.edited();
        assert_eq!(status.message(), SAVE_PROMPT);
        status.account_saved();
        assert_eq!(status.message(), SAVE_PROMPT);

        status.details_saved();
        assert_eq!(status.message(), "Settings saved");
        status.account_saved();
        assert_eq!(status.message(), "Codex account saved");
        status.edited();
        assert_eq!(status.message(), SAVE_PROMPT);
        // Nor does a delegation choice.
        status.delegation_saved();
        assert_eq!(status.message(), SAVE_PROMPT);
    }

    /// Each orchestrator choice is saved the moment it is picked, and a Project Settings
    /// opened afresh from the store shows it chosen.
    #[gpui::test]
    fn each_orchestrator_choice_saves_at_once_and_reopens_as_itself(cx: &mut gpui::TestAppContext) {
        use crate::form_input::{test_ax_node, test_turn, test_window};
        use gpui_kit::test::TestWindowExt as _;
        let dir = std::env::temp_dir().join(format!(
            "riwork-project-orchestrator-{}",
            uuid::Uuid::new_v4()
        ));
        let root = dir.join("demo");
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::open(dir.join("state")).unwrap();
        let project = store.add_project(&root, Some("Demo")).unwrap();
        // No account discovery in a test.
        cx.update(|app| {
            app.set_global(CodexAccountsState {
                snapshot: None,
                pending: true,
            })
        });
        let open = |cx: &mut gpui::TestAppContext| {
            let store = store.clone();
            let project = store
                .snapshot()
                .unwrap()
                .project(&project.id)
                .unwrap()
                .clone();
            test_window(cx, move |window, cx| {
                ProjectSettingsPanel::new(store, project, window, cx)
            })
        };
        let saved = || {
            store
                .snapshot()
                .unwrap()
                .project(&project.id)
                .unwrap()
                .orchestrator
                .clone()
        };
        let checked = |window: &gpui::Window, app: &App, id: &str| {
            test_ax_node(window, app, id.to_owned()).toggled()
                == Some(gpui::accesskit::Toggled::True)
        };
        let reopened_shows = |cx: &mut gpui::TestAppContext, ids: &[&str]| {
            let (handle, _) = open(cx);
            let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
            test_turn(cx, handle, move |window, app| {
                for id in &ids {
                    assert!(checked(window, app, id), "{id} is shown chosen");
                }
            });
        };

        let (handle, panel) = open(cx);
        test_turn(cx, handle, |window, app| {
            // A project that never chose inherits, and says what it inherits.
            assert!(checked(window, app, "project-orchestrator-inherit"));
            assert_eq!(
                test_ax_node(window, app, "project-orchestrator-inherit").label(),
                Some("Inherit app default · Automatic")
            );
            assert!(
                window
                    .try_find("project-orchestrator-provider-muse")
                    .is_none()
            );
            window.click("project-orchestrator-automatic", app);
        });
        assert_eq!(
            saved(),
            ProjectOrchestrator::Choose(OrchestratorRuns::Automatic)
        );
        reopened_shows(cx, &["project-orchestrator-automatic"]);

        test_turn(cx, handle, |window, app| {
            window.click("project-orchestrator-terminal", app)
        });
        assert_eq!(
            saved(),
            ProjectOrchestrator::Choose(OrchestratorRuns::Terminal)
        );
        reopened_shows(cx, &["project-orchestrator-terminal"]);

        // Chat starts as Settings' chat (Codex, its default model), then is changed here.
        test_turn(cx, handle, |window, app| {
            window.click("project-orchestrator-chat", app)
        });
        assert_eq!(
            saved(),
            ProjectOrchestrator::Choose(OrchestratorRuns::chat(Provider::Codex))
        );
        test_turn(cx, handle, |window, app| {
            assert!(checked(window, app, "project-orchestrator-model-default"));
            window.click("project-orchestrator-provider-muse", app)
        });
        test_turn(cx, handle, |window, app| {
            window.click("project-orchestrator-model-recommended", app)
        });
        let wonder = ProjectOrchestrator::Choose(OrchestratorRuns::Chat {
            provider: Provider::Muse,
            model: Some("muse-wonder-full-1.0".into()),
        });
        assert_eq!(saved(), wonder);
        reopened_shows(
            cx,
            &[
                "project-orchestrator-chat",
                "project-orchestrator-provider-muse",
                "project-orchestrator-model-recommended",
            ],
        );

        // Another model ID, typed and submitted with Return.
        test_turn(cx, handle, |window, app| {
            window.click("project-orchestrator-model-input", app);
            panel.update(app, |form, cx| {
                crate::form_input::set_value(
                    &form.orchestrator_model_state,
                    "muse-spark-1.3".into(),
                    window,
                    cx,
                );
            });
        });
        test_turn(cx, handle, |window, app| window.press("enter", app));
        assert_eq!(
            saved(),
            ProjectOrchestrator::Choose(OrchestratorRuns::Chat {
                provider: Provider::Muse,
                model: Some("muse-spark-1.3".into()),
            })
        );
        reopened_shows(cx, &["project-orchestrator-model-saved"]);
        // A blank one is refused and changes nothing.
        test_turn(cx, handle, |window, app| {
            panel.update(app, |form, cx| form.use_typed_model(window, cx));
        });
        test_turn(cx, handle, |_, app| {
            assert!(panel.read(app).orchestrator_error.is_some());
        });
        assert!(matches!(
            saved(),
            ProjectOrchestrator::Choose(OrchestratorRuns::Chat { .. })
        ));

        // Inherit again.
        test_turn(cx, handle, |window, app| {
            window.click("project-orchestrator-inherit", app)
        });
        assert_eq!(saved(), ProjectOrchestrator::Inherit);
        reopened_shows(cx, &["project-orchestrator-inherit"]);

        // A value this build cannot read: no mode is shown chosen, the panel says why,
        // and nothing is rewritten until a choice is made.
        store
            .set_project_orchestrator(
                &project.id,
                ProjectOrchestrator::Unreadable(serde_json::json!({"mode": "hologram"})),
            )
            .unwrap();
        let (handle, panel) = open(cx);
        test_turn(cx, handle, |window, app| {
            for (_, _, id) in ORCHESTRATOR_MODES {
                assert!(!checked(window, app, id), "{id}");
            }
            assert!(
                panel
                    .read(app)
                    .project
                    .orchestrator
                    .unreadable_note()
                    .is_some()
            );
            assert!(
                window
                    .try_find("project-orchestrator-model-input")
                    .is_none()
            );
        });
        assert!(matches!(saved(), ProjectOrchestrator::Unreadable(_)));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Each delegation choice is saved for this project the moment it is picked, a
    /// Project Settings opened afresh shows it chosen, and Inherit clears it again. The
    /// Inherit chips name Settings' values.
    #[gpui::test]
    fn each_delegation_choice_saves_at_once_for_this_project_and_reopens_as_itself(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::form_input::{test_ax_node, test_turn, test_window};
        use gpui_kit::test::TestWindowExt as _;
        let dir = std::env::temp_dir().join(format!(
            "riwork-project-delegation-{}",
            uuid::Uuid::new_v4()
        ));
        let (root, other_root) = (dir.join("demo"), dir.join("other"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other_root).unwrap();
        let store = Store::open(dir.join("state")).unwrap();
        let project = store.add_project(&root, Some("Demo")).unwrap();
        let other = store.add_project(&other_root, Some("Other")).unwrap();
        // No account discovery in a test.
        cx.update(|app| {
            app.set_global(CodexAccountsState {
                snapshot: None,
                pending: true,
            })
        });
        // Settings choose six worker shells and Muse as the workhorse; the rest is built in.
        let mut settings = Settings::default();
        settings.delegation_policy.set_parallel_shells(Some(6));
        settings
            .delegation_policy
            .set_route(Work::Workhorse, Some(Route::recommended(Provider::Muse)));
        let open = |cx: &mut gpui::TestAppContext| {
            let store = store.clone();
            let project = store
                .snapshot()
                .unwrap()
                .project(&project.id)
                .unwrap()
                .clone();
            let (handle, panel) = test_window(cx, move |window, cx| {
                ProjectSettingsPanel::new(store, project, window, cx)
            });
            let settings = settings.clone();
            cx.update(|app| app.set_global(settings));
            // The section sits below the first screen of the test window.
            test_turn(cx, handle, |window, app| {
                window.scroll(
                    "project-folder-unfiled",
                    gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-4000.))),
                    app,
                );
            });
            (handle, panel)
        };
        let saved = |id: &str| {
            store
                .snapshot()
                .unwrap()
                .project(id)
                .unwrap()
                .delegation_policy
                .clone()
        };
        let checked = |window: &gpui::Window, app: &App, id: &str| {
            test_ax_node(window, app, id.to_owned()).toggled()
                == Some(gpui::accesskit::Toggled::True)
        };
        let reopened_shows = |cx: &mut gpui::TestAppContext, ids: &[&str]| {
            let (handle, _) = open(cx);
            let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
            test_turn(cx, handle, move |window, app| {
                for id in &ids {
                    assert!(checked(window, app, id), "{id} is shown chosen");
                }
            });
        };
        let inherit = [
            "project-delegation-spread-inherit",
            "project-delegation-parallel-inherit",
            "project-delegation-design-inherit",
            "project-delegation-workhorse-inherit",
        ];

        let (handle, panel) = open(cx);
        test_turn(cx, handle, |window, app| {
            // A project that never chose inherits each field, and says what it inherits.
            for (id, label) in inherit.into_iter().zip([
                "Inherit · On",
                "Inherit · 6",
                "Inherit · Claude · opus",
                "Inherit · Muse · muse-wonder-full-1.0",
            ]) {
                assert!(checked(window, app, id), "{id}");
                assert_eq!(test_ax_node(window, app, id).label(), Some(label));
            }
            for id in [
                "project-delegation-spread-off",
                "project-delegation-parallel-more",
                "project-delegation-design-codex",
            ] {
                assert!(window.find(id).visible(), "{id} is drawn in view");
            }
            assert!(window.try_find("project-delegation-design-saved").is_none());
            window.click("project-delegation-spread-off", app);
        });
        assert_eq!(saved(&project.id).spread_work, Some(false));
        test_turn(cx, handle, |_, app| {
            assert_eq!(panel.read(app).status.message(), "Delegation policy saved");
        });
        reopened_shows(cx, &["project-delegation-spread-off"]);

        // The stepper starts from Settings' six while inheriting.
        test_turn(cx, handle, |window, app| {
            window.click("project-delegation-parallel-more", app)
        });
        assert_eq!(saved(&project.id).parallel_shells, Some(7));
        test_turn(cx, handle, |window, app| {
            assert!(!checked(window, app, "project-delegation-parallel-inherit"));
            window.click("project-delegation-parallel-fewer", app);
        });
        test_turn(cx, handle, |window, app| {
            window.click("project-delegation-parallel-fewer", app)
        });
        assert_eq!(saved(&project.id).parallel_shells, Some(5));
        let (reopened, _) = open(cx);
        test_turn(cx, reopened, |window, app| {
            assert!(!checked(window, app, "project-delegation-parallel-inherit"));
        });

        test_turn(cx, handle, |window, app| {
            window.click("project-delegation-design-codex", app)
        });
        test_turn(cx, handle, |window, app| {
            window.click("project-delegation-workhorse-claude", app)
        });
        let policy = saved(&project.id);
        assert_eq!(policy.design, Some(Route::recommended(Provider::Codex)));
        assert_eq!(policy.workhorse, Some(Route::recommended(Provider::Claude)));
        reopened_shows(
            cx,
            &[
                "project-delegation-spread-off",
                "project-delegation-design-codex",
                "project-delegation-workhorse-claude",
            ],
        );
        // For this project only.
        assert!(saved(&other.id).is_empty());

        // Inherit again clears each field.
        for id in inherit {
            test_turn(cx, handle, |window, app| window.click(id, app));
        }
        assert!(saved(&project.id).is_empty(), "{:?}", saved(&project.id));
        reopened_shows(cx, &inherit);

        // A route the chips do not offer is shown chosen as saved, and a value this build
        // cannot read is named while Inherit stands in for it.
        store
            .set_project_delegation_policy(&project.id, |policy| {
                policy.set_route(
                    Work::Design,
                    Some(Route {
                        provider: Provider::Claude,
                        model: "fable".into(),
                    }),
                );
                policy
                    .unreadable
                    .insert("spread_work".into(), serde_json::json!("often"));
            })
            .unwrap();
        let (handle, panel) = open(cx);
        test_turn(cx, handle, |window, app| {
            assert!(checked(window, app, "project-delegation-design-saved"));
            assert_eq!(
                test_ax_node(window, app, "project-delegation-design-saved").label(),
                Some("Claude · fable · saved")
            );
            assert!(checked(window, app, "project-delegation-spread-inherit"));
            let note = panel
                .read(app)
                .project
                .delegation_policy
                .unreadable_note("This project's")
                .unwrap();
            assert!(note.contains(r#"spread_work = "often""#), "{note}");
            // Choosing replaces what could not be read.
            window.click("project-delegation-spread-on", app);
        });
        let policy = saved(&project.id);
        assert_eq!(policy.spread_work, Some(true));
        assert!(policy.unreadable.is_empty());

        // Native draws the same controls, which save the same way.
        let before = ui_text::set_for_tests(1.0, ui_text::Face::System);
        let (handle, _) = open(cx);
        test_turn(cx, handle, |window, app| {
            assert!(checked(window, app, "project-delegation-spread-on"));
            assert!(checked(window, app, "project-delegation-design-saved"));
            assert_eq!(
                test_ax_node(window, app, "project-delegation-parallel-inherit").label(),
                Some("Inherit · 6")
            );
            assert!(window.find("project-delegation-parallel-more").visible());
            window.click("project-delegation-workhorse-codex", app);
        });
        ui_text::set_for_tests(before.0, before.1);
        assert_eq!(
            saved(&project.id).workhorse,
            Some(Route::recommended(Provider::Codex))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The memo is the last section: a chat's Edit brings it into view with the keys, typed
    /// text saves by itself for this project only, and a save from the phone shows on the next
    /// refresh.
    #[gpui::test]
    fn the_memo_section_saves_itself_and_follows_the_phone(cx: &mut gpui::TestAppContext) {
        use crate::form_input::{test_turn, test_window};
        use gpui_kit::test::TestWindowExt as _;
        let dir =
            std::env::temp_dir().join(format!("riwork-project-memo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("demo")).unwrap();
        std::fs::create_dir_all(dir.join("other")).unwrap();
        let store = Store::open(dir.join("state")).unwrap();
        let project = store.add_project(dir.join("demo"), Some("Demo")).unwrap();
        let other = store.add_project(dir.join("other"), Some("Other")).unwrap();
        cx.update(|app| {
            app.set_global(CodexAccountsState {
                snapshot: None,
                pending: true,
            })
        });
        let (handle, panel) = {
            let (store, project) = (store.clone(), project.clone());
            test_window(cx, move |window, cx| {
                ProjectSettingsPanel::new(store, project, window, cx)
            })
        };
        test_turn(cx, handle, |window, app| {
            assert!(window.try_find("project-memo-editor").is_some());
            panel.update(app, |panel, cx| panel.reveal_memo(window, cx));
        });
        test_turn(cx, handle, |window, app| {
            assert!(panel.read(app).active == Field::Memo);
            // Scrolled into view: a click reaches it (one off screen panics).
            window.click("project-memo-editor", app);
            window.input("Ship it", app);
        });
        cx.executor().advance_clock(crate::memo_editor::SAVE_DELAY);
        cx.run_until_parked();
        let state = store.snapshot().unwrap();
        assert_eq!(state.project_memo(&project.id).text, "Ship it");
        assert!(state.project_memo(&other.id).is_empty());
        // Typing in the memo is not an unsaved name or folder edit.
        assert_eq!(panel.read_with(cx, |panel, _| panel.status.pending), false);

        store
            .set_project_memo(&project.id, "From the phone")
            .unwrap();
        test_turn(cx, handle, |window, app| {
            panel.update(app, |panel, cx| panel.refresh_folders(window, cx));
        });
        panel.read_with(cx, |panel, cx| {
            assert_eq!(panel.memo.read(cx).text(), "From the phone");
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_multi_line_input_keeps_its_line_breaks_where_a_single_line_one_folds_them() {
        let mut lines = Input::new("a".into());
        lines.replace_lines(None, "b\r\nc\rd\u{2028}e");
        assert_eq!(lines.text, "ab\nc\nd\ne");
        assert_eq!(lines.selection, 8..8);

        let mut line = Input::new("a".into());
        line.replace(None, "b\nc");
        assert_eq!(line.text, "ab c");
    }
}
