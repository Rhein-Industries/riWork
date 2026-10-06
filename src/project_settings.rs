//! Project metadata lives in a project tab; virtual folders never move files.

use std::{collections::BTreeMap, ops::Range};

use crate::text_input::{self, EnterBehavior, InputEvent, InputState};
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, HighlightStyle, IntoElement, KeyDownEvent,
    MouseButton, Pixels, Point, Render, StyledText, UTF16Selection, Window, canvas, div,
    prelude::*, rgb,
};
use gpui::{Focusable, Subscription};

use crate::{
    behavior_controls as behavior,
    icons::{self, ActionGlyph, Icon},
    settings::{CodexAccountsState, Settings, refresh_codex_accounts},
    store::{Project, ProjectCodexAccount, ProjectFolder, State, Store},
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
    Save,
}

pub struct ProjectSettingsPanel {
    store: Store,
    project: Project,
    name: Input,
    name_state: Entity<InputState>,
    name_touched: bool,
    _input_subscriptions: Vec<Subscription>,
    folder_name: Input,
    folder_name_state: Entity<InputState>,
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
        let mut subscriptions = Vec::new();
        for (field, state) in [
            (Field::Name, &name_state),
            (Field::FolderName, &folder_name_state),
        ] {
            subscriptions.push(cx.subscribe_in(
                state,
                window,
                move |form, state, event, window, cx| match event {
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
            _input_subscriptions: subscriptions,
            name: Input::new(project.name.clone()),
            folder_id: project.folder_id.clone(),
            project,
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
        controls.extend([
            ("project-settings-create-folder".into(), Field::AddFolder),
            ("project-codex-refresh".into(), Field::AccountRefresh),
            ("project-settings-save".into(), Field::Save),
        ]);
        controls
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active {
            Field::Name => self.name_state.read(cx).focus_handle(cx).focus(window, cx),
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

    /// Refresh choices without replacing the user's partially edited project name.
    pub fn refresh_folders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.store.snapshot() {
            Ok(state) => {
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
        matches!(self.active, Field::Name | Field::FolderName)
    }

    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active {
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
            _ => None,
        };
        if matches!(event.keystroke.key.as_str(), "escape" | "tab")
            && input.is_some_and(|state| crate::form_input::is_composing(state, window, cx))
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
            "s" if event.keystroke.modifiers.platform => {
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
                order.push(Field::Save);
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
            _ => unreachable!("Only text fields render an input"),
        };
        crate::form_input::frame(id, state, false, window, cx).into_any_element()
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
                    let listener = cx.listener(move |form, _, window, cx| {
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
                .on_click(cx.listener(|form, _, window, cx| {
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
            .child(section("04  LOCATIONS", colors))
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
                                .child(save)
                                .child(locations),
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
                    .child(save)
                    .child(locations),
            )
            .into_any_element()
    }
}

/// Native's choice among a few options (a folder, an account): a capsule, filled with the
/// primary color when chosen. The colorful themes keep their outlined boxes.
fn native_choice<E: gpui::Styled + gpui::InteractiveElement>(
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
            .aria_label(if self.folder.is_some() { "Rename virtual folder" } else if self.parent_id.is_some() { "New subfolder" } else { "New virtual folder" })
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

pub(crate) use impl_input_handler;

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
