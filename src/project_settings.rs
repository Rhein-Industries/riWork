//! Project metadata lives in a project tab; virtual folders never move files.

use std::{collections::BTreeMap, ops::Range};

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, HighlightStyle, IntoElement, KeyDownEvent,
    MouseButton, Pixels, Point, Render, StyledText, UTF16Selection, Window, canvas, div,
    prelude::*, rgb,
};

use crate::{
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

    fn cursor(&self) -> usize {
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
        let range = range
            .map(|range| {
                utf16_to_byte(&self.text, range.start)..utf16_to_byte(&self.text, range.end)
            })
            .or(self.marked.take())
            .unwrap_or_else(|| self.selection.clone());
        let text = single_line(text);
        self.text.replace_range(range.clone(), &text);
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

/// These inputs are single-line. A line break between pasted lines becomes one
/// space so words do not fuse; a copied line's trailing break is dropped.
fn single_line(text: &str) -> String {
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
        .h(ui_text::space(34.0))
        .px(ui_text::space(10.0))
        .flex()
        .items_center()
        .min_w_0()
        .bg(rgb(colors.bg))
        .border_1()
        .border_color(rgb(if active { colors.cyan } else { colors.divider }))
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
            // Native: a plain semibold title, without the number or the rule.
            crate::controls::native(heading, |heading| {
                heading
                    .border_b_0()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(colors.text))
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
    folder_name: Input,
    folders: Vec<ProjectFolder>,
    folder_paths: BTreeMap<String, String>,
    folder_id: Option<String>,
    active: Field,
    focus: FocusHandle,
    error: Option<String>,
    status: SaveStatus,
}

impl EventEmitter<ProjectSettingsEvent> for ProjectSettingsPanel {}

impl ProjectSettingsPanel {
    pub fn new(store: Store, project: Project, cx: &mut Context<Self>) -> Self {
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
        Self {
            name: Input::new(project.name.clone()),
            folder_id: project.folder_id.clone(),
            project,
            store,
            folders,
            folder_paths,
            folder_name: Input::default(),
            active: Field::Name,
            focus: cx.focus_handle(),
            error,
            status: SaveStatus::default(),
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    /// Refresh choices without replacing the user's partially edited project name.
    pub fn refresh_folders(&mut self, cx: &mut Context<Self>) {
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
                    if self.name.text.trim() == self.project.name
                        && project.name != self.project.name
                    {
                        self.name = Input::new(project.name.clone());
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

    fn input(&self) -> &Input {
        match self.active {
            Field::FolderName => &self.folder_name,
            _ => &self.name,
        }
    }

    fn input_mut(&mut self) -> &mut Input {
        match self.active {
            Field::FolderName => &mut self.folder_name,
            _ => &mut self.name,
        }
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.status.edited();
        self.error = None;
        cx.notify();
    }

    fn accepts_input(&self) -> bool {
        matches!(self.active, Field::Name | Field::FolderName)
    }

    fn activate(&mut self, cx: &mut Context<Self>) {
        match self.active {
            Field::Name | Field::Save => self.save(cx),
            Field::FolderName | Field::AddFolder => self.create_folder(cx),
            Field::Folder(index) => {
                self.folder_id = index
                    .checked_sub(1)
                    .and_then(|index| self.folders.get(index))
                    .map(|folder| folder.id.clone());
                self.edited(cx);
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

    fn save(&mut self, cx: &mut Context<Self>) {
        match self.store.update_project_metadata(
            &self.project.id,
            self.name.text.trim(),
            self.folder_id.as_deref(),
        ) {
            Ok(project) => {
                self.name = Input::new(project.name.clone());
                self.project = project.clone();
                self.error = None;
                self.status.details_saved();
                cx.emit(ProjectSettingsEvent::Saved(project));
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn create_folder(&mut self, cx: &mut Context<Self>) {
        match self
            .store
            .create_project_folder_in(self.folder_name.text.trim(), self.folder_id.as_deref())
        {
            Ok(folder) => {
                self.folder_id = Some(folder.id.clone());
                self.folder_name = Input::default();
                self.folders.push(folder);
                self.active = Field::Name;
                self.error = None;
                self.status.edited();
                self.refresh_folders(cx);
                cx.emit(ProjectSettingsEvent::FolderChanged);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let handled = match event.keystroke.key.as_str() {
            "enter" | "return" => {
                self.activate(cx);
                true
            }
            "space" if !self.accepts_input() => {
                self.activate(cx);
                true
            }
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
            "left" | "up" if matches!(self.active, Field::Account(_)) => {
                if let Field::Account(index) = self.active {
                    let len = self.account_choices(cx).len();
                    self.active = Field::Account(index.checked_sub(1).unwrap_or(len - 1));
                }
                true
            }
            "right" | "down" if matches!(self.active, Field::Account(_)) => {
                if let Field::Account(index) = self.active {
                    self.active = Field::Account((index + 1) % self.account_choices(cx).len());
                }
                true
            }
            "s" if event.keystroke.modifiers.platform => {
                self.save(cx);
                true
            }
            "tab" => {
                let last_folder = self.folders.len();
                let last_account = self.account_choices(cx).len() - 1;
                self.active = if event.keystroke.modifiers.shift {
                    match self.active {
                        Field::Name => Field::Save,
                        Field::Folder(0) => Field::Name,
                        Field::Folder(index) => Field::Folder(index - 1),
                        Field::FolderName => Field::Folder(last_folder),
                        Field::AddFolder => Field::FolderName,
                        Field::Account(0) => Field::AddFolder,
                        Field::Account(index) => Field::Account(index - 1),
                        Field::AccountRefresh => Field::Account(last_account),
                        Field::Save => Field::AccountRefresh,
                    }
                } else {
                    match self.active {
                        Field::Name => Field::Folder(0),
                        Field::Folder(index) if index < last_folder => Field::Folder(index + 1),
                        Field::Folder(_) => Field::FolderName,
                        Field::FolderName => Field::AddFolder,
                        Field::AddFolder => Field::Account(0),
                        Field::Account(index) if index < last_account => Field::Account(index + 1),
                        Field::Account(_) => Field::AccountRefresh,
                        Field::AccountRefresh => Field::Save,
                        Field::Save => Field::Name,
                    }
                };
                self.focus.focus(window, cx);
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

    fn field(&self, field: Field, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let (input, placeholder, id) = match field {
            Field::Name => (&self.name, "Project name", "project-settings-name"),
            Field::FolderName => (
                &self.folder_name,
                if self.folder_id.is_some() {
                    "New subfolder"
                } else {
                    "New virtual folder"
                },
                "project-settings-new-folder",
            ),
            _ => unreachable!("Only text fields render an input"),
        };
        div()
            .id(id)
            .flex_1()
            .min_w_0()
            .child(input_content(
                input,
                self.active == field && self.focus.is_focused(window),
                placeholder,
                &self.focus,
                cx.entity(),
                colors,
            ))
            .on_click(cx.listener(move |form, _, window, cx| {
                form.active = field;
                form.focus.focus(window, cx);
                cx.notify();
            }))
            .into_any_element()
    }
}

impl Render for ProjectSettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let dirty =
            self.name.text.trim() != self.project.name || self.folder_id != self.project.folder_id;
        let focused = self.focus.is_focused(window);
        let mut folders = div().flex().flex_wrap().gap(ui_text::space(6.0)).child(
            div()
                .id("project-folder-unfiled")
                .px(ui_text::space(10.0))
                .py(ui_text::space(7.0))
                .cursor_pointer()
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
                .child(ui_text::cased("Unfiled"))
                .on_click(cx.listener(|form, _, window, cx| {
                    form.active = Field::Folder(0);
                    form.focus.focus(window, cx);
                    form.folder_id = None;
                    form.edited(cx);
                })),
        );
        for (index, folder) in self.folders.iter().enumerate() {
            let id = folder.id.clone();
            let selected = self.folder_id.as_deref() == Some(folder.id.as_str());
            folders = folders.child(
                div()
                    .id(format!("project-folder-{}", folder.id))
                    .px(ui_text::space(10.0))
                    .py(ui_text::space(7.0))
                    .cursor_pointer()
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
                    .child(
                        self.folder_paths
                            .get(&folder.id)
                            .cloned()
                            .unwrap_or_else(|| folder.name.clone()),
                    )
                    .on_click(cx.listener(move |form, _, window, cx| {
                        form.active = Field::Folder(index + 1);
                        form.focus.focus(window, cx);
                        form.folder_id = Some(id.clone());
                        form.edited(cx);
                    })),
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
                        .child(root.to_string_lossy().into_owned()),
                );
            }
        }
        let account_state = cx.global::<CodexAccountsState>().clone();
        let mut account_rows = div().flex().flex_wrap().gap(ui_text::space(6.0));
        for (index, (choice, label, available)) in self.account_choices(cx).into_iter().enumerate()
        {
            let selected = choice == self.project.codex_account;
            let id = match &choice {
                ProjectCodexAccount::Inherit => "project-codex-inherit".to_owned(),
                ProjectCodexAccount::SystemDefault => "project-codex-system".to_owned(),
                ProjectCodexAccount::Saved(id) => format!("project-codex-saved-{id}"),
            };
            account_rows = account_rows.child(
                div()
                    .id(id)
                    .max_w(ui_text::space(300.0))
                    .min_w_0()
                    .px(ui_text::space(10.0))
                    .py(ui_text::space(7.0))
                    .flex()
                    .items_center()
                    .gap(ui_text::space(7.0))
                    .cursor_pointer()
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
                    .child(if selected { "◉" } else { "○" })
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(label),
                    )
                    .on_click(cx.listener(move |form, _, window, cx| {
                        form.active = Field::Account(index);
                        form.focus.focus(window, cx);
                        if available {
                            form.select_account(choice.clone(), cx);
                        }
                    })),
            );
        }
        div()
            .id("project-settings-panel")
            .size_full().min_w_0().track_focus(&self.focus).key_context("ProjectSettings")
            .on_key_down(cx.listener(Self::key_down)).overflow_y_scroll()
            .bg(rgb(colors.bg)).text_color(rgb(colors.text)).font_family(ui_text::ui_family()).text_size(ui_text::text(11.0))
            .p(ui_text::space(20.0))
            .child(
                div().w_full().min_w_0().max_w(ui_text::space(760.0)).flex().flex_col().gap(ui_text::space(18.0))
                    .child(
                        div().flex().flex_wrap().min_w_0().justify_between().items_center().gap(ui_text::space(12.0))
                            .border_l_2().border_color(rgb(colors.cyan)).pl(ui_text::space(12.0)).py(ui_text::space(6.0))
                            .child(div().min_w_0().flex().flex_col().gap(ui_text::space(5.0))
                                .child(div().text_color(rgb(colors.cyan)).text_size(ui_text::text(16.0)).when(ui_text::is_native(), |title| title.font_weight(gpui::FontWeight::SEMIBOLD)).child(ui_text::cased("Project settings")))
                                .child(div().min_w_0().overflow_hidden().text_ellipsis().text_color(rgb(colors.muted)).text_size(ui_text::text(10.0)).child(self.project.name.clone())))
                            .child(div().px(ui_text::space(8.0)).py(ui_text::space(4.0)).border_1().border_color(rgb(colors.divider))
                                .text_size(ui_text::text(9.0)).text_color(rgb(if dirty { colors.magenta } else { colors.muted }))
                                .child(ui_text::cased(if dirty { "Unsaved" } else { "Local project" }))),
                    )
                    .child(
                        div().p(ui_text::space(14.0)).flex().flex_col().gap(ui_text::space(12.0)).bg(rgb(colors.panel))
                            .border_1().border_color(rgb(colors.divider))
                            .child(section("01  IDENTITY", colors))
                            .child(div().text_color(rgb(colors.muted)).text_size(ui_text::text(10.0)).child(ui_text::cased("Project name")))
                            .child(self.field(Field::Name, window, cx)),
                    )
                    .child(
                        div().p(ui_text::space(14.0)).flex().flex_col().gap(ui_text::space(12.0)).bg(rgb(colors.panel))
                            .border_1().border_color(rgb(colors.divider))
                            .child(section("02  VIRTUAL FOLDER", colors))
                            .child(folders)
                            .child(div().text_color(rgb(colors.muted)).text_size(ui_text::text(10.0)).child("Group projects in the browser. Files stay in their current locations."))
                            .children(self.folder_id.as_ref().and_then(|id| self.folder_paths.get(id)).map(|path| {
                                div().text_color(rgb(colors.magenta)).text_size(ui_text::text(10.0)).child(format!("NEW SUBFOLDER UNDER  /  {path}"))
                            }))
                            .child(div().flex().flex_wrap().min_w_0().items_center().gap(ui_text::space(8.0))
                                .child(self.field(Field::FolderName, window, cx))
                                .child({
                                    let subfolder = self.folder_id.is_some();
                                    let button = div().id("project-settings-create-folder").h(ui_text::space(34.0))
                                        .flex_none().flex().items_center().cursor_pointer().border_1()
                                        .border_color(rgb(if focused && self.active == Field::AddFolder { colors.focus } else { colors.magenta }))
                                        .text_color(rgb(colors.magenta));
                                    // Icons keep the button's size, colour and focus ring; the tooltip names it.
                                    let button = if icons::labels_as_icons(cx) {
                                        button.w(ui_text::space(34.0)).justify_center()
                                            .child(icons::icon(Icon::Action(ActionGlyph::NewFolder), colors.magenta))
                                            .child(tooltip::anchor(if subfolder { "New subfolder" } else { "New folder" }, Look::Control))
                                    } else {
                                        button.px(ui_text::space(12.0)).child(if subfolder { "+ SUBFOLDER" } else { "+ FOLDER" })
                                    };
                                    button.on_click(cx.listener(|form, _, window, cx| {
                                        form.active = Field::AddFolder;
                                        form.focus.focus(window, cx);
                                        form.create_folder(cx);
                                    }))
                                })),
                    )
                    .child(
                        div().p(ui_text::space(14.0)).flex().flex_col().gap(ui_text::space(10.0)).bg(rgb(colors.panel))
                            .border_1().border_color(rgb(colors.divider))
                            .child(section("03  CODEX ACCOUNT", colors))
                            .child(div().text_size(ui_text::text(10.0)).text_color(rgb(colors.muted))
                                .child("New Codex sessions use this choice. Running sessions keep their account. Selection saves immediately."))
                            .child(account_rows)
                            .child(div().id("project-codex-refresh").cursor_pointer()
                                .text_size(ui_text::text(10.0))
                                .text_color(rgb(if focused && self.active == Field::AccountRefresh { colors.focus } else { colors.cyan }))
                                .child(ui_text::cased(if account_state.pending { "Checking accounts…" } else { "Refresh accounts" }))
                                .on_click(cx.listener(|form, _, window, cx| {
                                    form.active = Field::AccountRefresh;
                                    form.focus.focus(window, cx);
                                    refresh_codex_accounts(cx);
                                })))
                            .children(account_state.snapshot.as_ref().and_then(|snapshot| snapshot.error.as_ref()).map(|error|
                                div().text_size(ui_text::text(10.0)).text_color(rgb(colors.gold)).child(error.clone())))
                            .children(matches!(self.project.codex_account, ProjectCodexAccount::Saved(_))
                                .then_some(self.project.codex_account.clone())
                                .filter(|choice| self.account_choices(cx).iter().any(|(row, _, available)| row == choice && !available))
                                .map(|_| div().text_size(ui_text::text(10.0)).text_color(rgb(colors.gold))
                                    .child("Selected account unavailable. Choose another before starting Codex."))),
                    )
                    .child(
                        div().flex().flex_wrap().min_w_0().items_center().justify_between().gap(ui_text::space(10.0))
                            .child(div().min_w_0().text_size(ui_text::text(10.0)).text_color(rgb(if self.error.is_some() { colors.gold } else { colors.muted }))
                                .child(self.error.clone().unwrap_or_else(|| self.status.message().into())))
                            .child(div().id("project-settings-save").flex_none().px(ui_text::space(14.0)).py(ui_text::space(10.0)).cursor_pointer()
                                .bg(rgb(colors.panel_active)).border_1()
                                .border_color(rgb(if focused && self.active == Field::Save { colors.focus } else { colors.cyan }))
                                .text_color(rgb(colors.cyan)).child(ui_text::cased("Save project"))
                                .on_click(cx.listener(|form, _, window, cx| {
                                    form.active = Field::Save;
                                    form.focus.focus(window, cx);
                                    form.save(cx);
                                }))),
                    )
                    .child(
                        div().p(ui_text::space(14.0)).flex().flex_col().gap(ui_text::space(12.0)).bg(rgb(colors.panel))
                            .border_1().border_color(rgb(colors.divider))
                            .child(section("04  LOCATIONS", colors))
                            .child(div().text_color(rgb(colors.muted)).text_size(ui_text::text(10.0)).child(ui_text::cased("Project root")))
                            .child(div().px(ui_text::space(10.0)).py(ui_text::space(8.0)).bg(rgb(colors.bg)).border_l_1()
                                .border_color(rgb(colors.cyan)).child(self.project.root.to_string_lossy().into_owned()))
                            .child(div().text_color(rgb(colors.muted)).text_size(ui_text::text(10.0))
                                .child(format!("REPOSITORIES  /  {:02}", self.project.repository_roots.len())))
                            .child(repositories)
                            .child(div().text_color(rgb(colors.muted)).text_size(ui_text::text(9.0)).child(format!("PROJECT ID  /  {}", self.project.id))),
                    ),
            )
            .into_any_element()
    }
}

pub struct FolderEditor {
    store: Store,
    folder: Option<ProjectFolder>,
    parent_id: Option<String>,
    parent_path: Option<String>,
    name: Input,
    focus: FocusHandle,
    active: usize,
    error: Option<String>,
}

impl EventEmitter<FolderEditorEvent> for FolderEditor {}

impl FolderEditor {
    pub fn new(store: Store, folder: Option<ProjectFolder>, cx: &mut Context<Self>) -> Self {
        Self::new_in(store, folder, None, cx)
    }

    pub fn new_in(
        store: Store,
        folder: Option<ProjectFolder>,
        parent_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
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
        Self {
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
            active: 0,
            error,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    fn input(&self) -> &Input {
        &self.name
    }
    fn input_mut(&mut self) -> &mut Input {
        &mut self.name
    }
    fn accepts_input(&self) -> bool {
        self.active == 0
    }
    fn edited(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
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

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let handled = match event.keystroke.key.as_str() {
            "escape" => {
                cx.emit(FolderEditorEvent::Cancelled);
                true
            }
            "enter" | "return" => {
                if self.active == 1 {
                    cx.emit(FolderEditorEvent::Cancelled);
                } else {
                    self.submit(cx);
                }
                true
            }
            "space" if !self.accepts_input() => {
                if self.active == 1 {
                    cx.emit(FolderEditorEvent::Cancelled);
                } else {
                    self.submit(cx);
                }
                true
            }
            "tab" => {
                self.active = (self.active
                    + if event.keystroke.modifiers.shift {
                        2
                    } else {
                        1
                    })
                    % 3;
                true
            }
            _ => {
                let handled = self.accepts_input() && self.name.key(event, cx);
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
}

impl Render for FolderEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        div()
            .id("virtual-folder-editor")
            .occlude()
            .track_focus(&self.focus)
            .key_context("FolderEditor")
            .on_key_down(cx.listener(Self::key_down))
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
                    .child(input_content(
                        &self.name,
                        self.active == 0 && self.focus.is_focused(window),
                        "Folder name",
                        &self.focus,
                        cx.entity(),
                        colors,
                    ))
                    .on_click(cx.listener(|form, _, window, cx| {
                        form.active = 0;
                        form.focus.focus(window, cx);
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
                        div()
                            .id("cancel-virtual-folder")
                            .px(ui_text::space(12.0))
                            .py(ui_text::space(8.0))
                            .cursor_pointer()
                            .border_1()
                            .border_color(rgb(
                                if self.active == 1 && self.focus.is_focused(window) {
                                    colors.focus
                                } else {
                                    colors.panel
                                },
                            ))
                            .text_color(rgb(colors.muted))
                            .child(ui_text::cased("Cancel"))
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(FolderEditorEvent::Cancelled)),
                            ),
                    )
                    .child(
                        div()
                            .id("save-virtual-folder")
                            .px(ui_text::space(12.0))
                            .py(ui_text::space(8.0))
                            .cursor_pointer()
                            .bg(rgb(colors.panel_active))
                            .border_1()
                            .border_color(rgb(
                                if self.active == 2 && self.focus.is_focused(window) {
                                    colors.focus
                                } else {
                                    colors.cyan
                                },
                            ))
                            .text_color(rgb(colors.cyan))
                            .child("SAVE FOLDER  ↵")
                            .on_click(cx.listener(|form, _, _, cx| form.submit(cx))),
                    ),
            )
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

impl_input_handler!(ProjectSettingsPanel);
impl_input_handler!(FolderEditor);

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
}
