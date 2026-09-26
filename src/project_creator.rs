//! Compact project creation with optional Git initialization.

use std::{ops::Range, path::PathBuf, time::Duration};

use gpui::{
    AnyElement, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    EventEmitter, FocusHandle, HighlightStyle, IntoElement, KeyDownEvent, MouseButton,
    PathPromptOptions, Pixels, Point, Render, StyledText, UTF16Selection, Window, canvas, div,
    prelude::*, px, rgb,
};

use crate::{
    BG, CYAN, DIVIDER, GOLD, MAGENTA, MUTED, PANEL, PANEL_ACTIVE, TEXT,
    store::{Project, ProjectInspection, Store},
    utf16_to_byte,
};

pub enum ProjectCreationEvent {
    Created(Project),
    Cancelled,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Path,
    Name,
}

#[derive(Default)]
struct Input {
    text: String,
    selection: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
}

impl Input {
    fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    fn move_cursor(&mut self, offset: usize, select: bool) {
        if select {
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
}

pub struct ProjectCreator {
    path: Input,
    name: Input,
    default_directory: PathBuf,
    path_follows_name: bool,
    active: Field,
    focus: FocusHandle,
    store: Store,
    inspection: Option<ProjectInspection>,
    generation: u64,
    inspecting: bool,
    creating: bool,
    init_git: bool,
    error: Option<String>,
}

impl EventEmitter<ProjectCreationEvent> for ProjectCreator {}

impl ProjectCreator {
    pub fn new(
        store: Store,
        default_directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let path = format!("{}/", default_directory.display());
        let end = path.len();
        Self {
            path: Input {
                text: path,
                selection: end..end,
                ..Default::default()
            },
            name: Input::default(),
            default_directory,
            path_follows_name: true,
            active: Field::Name,
            focus,
            store,
            inspection: None,
            generation: 0,
            inspecting: false,
            creating: false,
            init_git: true,
            error: None,
        }
    }

    fn input(&self) -> &Input {
        match self.active {
            Field::Path => &self.path,
            Field::Name => &self.name,
        }
    }

    fn input_mut(&mut self) -> &mut Input {
        match self.active {
            Field::Path => &mut self.path,
            Field::Name => &mut self.name,
        }
    }

    fn path_value(&self) -> PathBuf {
        let text = self.path.text.trim();
        if let Some(tail) = text.strip_prefix("~/")
            && let Some(home) = std::env::var_os("HOME")
        {
            PathBuf::from(home).join(tail)
        } else {
            PathBuf::from(text)
        }
    }

    fn inspect(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.inspection = None;
        self.error = None;
        if self.path_follows_name {
            let name = self.name.text.trim();
            let path = if name.is_empty() {
                Ok(self.default_directory.clone())
            } else {
                crate::paths::default_new_project_path(name)
            };
            match path {
                Ok(path) => {
                    self.path.text = path.to_string_lossy().into_owned();
                    if name.is_empty() {
                        self.path.text.push('/');
                    }
                    let end = self.path.text.len();
                    self.path.selection = end..end;
                    self.path.reversed = false;
                    self.path.marked = None;
                }
                Err(error) => {
                    self.error = Some(error);
                    self.inspecting = false;
                    cx.notify();
                    return;
                }
            }
            if name.is_empty() {
                self.inspecting = false;
                cx.notify();
                return;
            }
        }
        if self.path.text.trim().is_empty() {
            self.inspecting = false;
            cx.notify();
            return;
        }
        self.inspecting = true;
        let path = self.path_value();
        let generation = self.generation;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor.timer(Duration::from_millis(250)).await;
            if !this
                .update(cx, |form, _| form.generation == generation)
                .unwrap_or(false)
            {
                return;
            }
            let result = executor
                .spawn(async move { Store::inspect_project(path) })
                .await;
            let _ = this.update(cx, |form, cx| {
                if form.generation != generation {
                    return;
                }
                form.inspecting = false;
                match result {
                    Ok(inspection) => form.inspection = Some(inspection),
                    Err(error) => form.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose project folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |form, window, cx| {
                    form.path_follows_name = false;
                    form.path.text = path.to_string_lossy().into_owned();
                    let end = form.path.text.len();
                    form.path.selection = end..end;
                    form.path.reversed = false;
                    form.path.marked = None;
                    form.active = Field::Path;
                    form.focus.focus(window, cx);
                    form.inspect(cx);
                });
            }
        })
        .detach();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.creating || self.inspecting || self.inspection.is_none() {
            return;
        }
        let path = self.path_value();
        let name = self.name.text.trim().to_owned();
        let init_git = self.init_git
            && self
                .inspection
                .as_ref()
                .is_some_and(|value| value.can_init_git);
        let store = self.store.clone();
        self.creating = true;
        self.error = None;
        let work = cx.background_executor().spawn(async move {
            store.create_project(path, (!name.is_empty()).then_some(name.as_str()), init_git)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |form, cx| {
                form.creating = false;
                match result {
                    Ok(project) => cx.emit(ProjectCreationEvent::Created(project)),
                    Err(error) => form.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn replace(&mut self, range: Option<Range<usize>>, text: &str, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        let input = self.input_mut();
        let range = range
            .map(|range| {
                utf16_to_byte(&input.text, range.start)..utf16_to_byte(&input.text, range.end)
            })
            .or(input.marked.take())
            .unwrap_or_else(|| input.selection.clone());
        let text = text.replace(['\n', '\r'], "");
        input.text.replace_range(range.clone(), &text);
        let end = range.start + text.len();
        input.selection = end..end;
        input.reversed = false;
        if self.active == Field::Path {
            self.path_follows_name = false;
        }
        self.inspect(cx);
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating {
            cx.stop_propagation();
            return;
        }
        let platform = event.keystroke.modifiers.platform;
        let handled = match event.keystroke.key.as_str() {
            "escape" => {
                cx.emit(ProjectCreationEvent::Cancelled);
                true
            }
            "enter" | "return" => {
                self.submit(cx);
                true
            }
            "tab" => {
                self.active = if self.active == Field::Path {
                    Field::Name
                } else {
                    Field::Path
                };
                self.focus.focus(window, cx);
                true
            }
            "g" if platform => {
                if self
                    .inspection
                    .as_ref()
                    .is_some_and(|value| value.can_init_git)
                {
                    self.init_git = !self.init_git;
                }
                true
            }
            "o" if platform => {
                self.browse(window, cx);
                true
            }
            "a" if platform => {
                let input = self.input_mut();
                input.selection = 0..input.text.len();
                input.reversed = false;
                true
            }
            "c" | "x" if platform => {
                let input = self.input();
                cx.write_to_clipboard(ClipboardItem::new_string(
                    input.text[input.selection.clone()].to_owned(),
                ));
                if event.keystroke.key == "x" {
                    self.replace(None, "", cx);
                }
                true
            }
            "v" if platform => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.replace(None, &text, cx);
                }
                true
            }
            "backspace" | "delete" => {
                let input = self.input_mut();
                if input.selection.is_empty() {
                    let cursor = input.selection.end;
                    input.selection = if event.keystroke.key == "backspace" {
                        input.text[..cursor]
                            .char_indices()
                            .next_back()
                            .map(|(offset, _)| offset)
                            .unwrap_or(0)..cursor
                    } else {
                        cursor
                            ..input.text[cursor..]
                                .chars()
                                .next()
                                .map(|ch| cursor + ch.len_utf8())
                                .unwrap_or(cursor)
                    };
                }
                self.replace(None, "", cx);
                true
            }
            "left" | "right" | "home" | "end" => {
                let input = self.input_mut();
                let cursor = input.cursor();
                let offset = match event.keystroke.key.as_str() {
                    "home" => 0,
                    "end" => input.text.len(),
                    "left" if platform => 0,
                    "right" if platform => input.text.len(),
                    "left" if !event.keystroke.modifiers.shift && !input.selection.is_empty() => {
                        input.selection.start
                    }
                    "right" if !event.keystroke.modifiers.shift && !input.selection.is_empty() => {
                        input.selection.end
                    }
                    "left" => input.text[..cursor]
                        .char_indices()
                        .next_back()
                        .map(|(offset, _)| offset)
                        .unwrap_or(0),
                    _ => input.text[cursor..]
                        .chars()
                        .next()
                        .map(|ch| cursor + ch.len_utf8())
                        .unwrap_or(cursor),
                };
                input.move_cursor(offset, event.keystroke.modifiers.shift);
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn render_field(
        &self,
        field: Field,
        label: &str,
        placeholder: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let input = match field {
            Field::Path => &self.path,
            Field::Name => &self.name,
        };
        let active = self.active == field;
        let input_handler = active.then(|| {
            let view = cx.entity();
            let focus = self.focus.clone();
            canvas(
                |_, _, _| {},
                move |bounds, _, window, cx| {
                    window.handle_input(&focus, ElementInputHandler::new(bounds, view.clone()), cx);
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
                        color: Some(rgb(CYAN).into()),
                        ..Default::default()
                    },
                ));
            } else {
                highlights.push((
                    input.selection.clone(),
                    HighlightStyle {
                        background_color: Some(rgb(DIVIDER).into()),
                        color: Some(rgb(CYAN).into()),
                        ..Default::default()
                    },
                ));
            }
        }
        div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .child(
                div()
                    .text_color(rgb(MUTED))
                    .text_size(px(9.0))
                    .child(label.to_owned()),
            )
            .child(
                div()
                    .id(if field == Field::Path {
                        "project-path"
                    } else {
                        "project-name"
                    })
                    .relative()
                    .h(px(32.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .min_w_0()
                    .bg(rgb(BG))
                    .border_1()
                    .border_color(rgb(if active { CYAN } else { DIVIDER }))
                    .text_color(rgb(if input.text.is_empty() { MUTED } else { TEXT }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(StyledText::new(text).with_highlights(highlights)),
                    )
                    .children(input_handler)
                    .on_click(cx.listener(move |form, _, window, cx| {
                        form.active = field;
                        form.focus.focus(window, cx);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
}

impl Render for ProjectCreator {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = !self.creating && !self.inspecting && self.inspection.is_some();
        let can_init = self
            .inspection
            .as_ref()
            .is_some_and(|value| value.can_init_git);
        let height = 260.0
            + if can_init
                || self
                    .inspection
                    .as_ref()
                    .is_some_and(|value| value.repository_count > 0)
            {
                28.0
            } else {
                0.0
            }
            + if self
                .inspection
                .as_ref()
                .is_some_and(|value| value.warning.is_some())
            {
                32.0
            } else {
                0.0
            }
            + if self.error.is_some() { 32.0 } else { 0.0 };
        let summary = if self.inspecting {
            "Checking folder…".to_owned()
        } else if let Some(inspection) = &self.inspection {
            match inspection.repository_count {
                0 if !inspection.exists => "New folder · no repositories".to_owned(),
                0 => "Folder · no repositories".to_owned(),
                1 => format!(
                    "1 Git repository · {}",
                    inspection.repository_roots[0].display()
                ),
                count => format!("Folder · {count} Git repositories"),
            }
        } else if self.path_follows_name {
            "Enter a project name, or choose an existing folder.".to_owned()
        } else {
            "Enter a folder path, or browse to an existing folder.".to_owned()
        };
        div()
            .id("project-creator")
            .occlude()
            .track_focus(&self.focus)
            .key_context("ProjectCreator")
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .w(px(540.0))
            .h(px(height))
            .flex_none()
            .max_w_full()
            .max_h(gpui::relative(0.9))
            .overflow_y_scroll()
            .p(px(16.0))
            .flex()
            .flex_col()
            .gap(px(14.0))
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(MAGENTA))
            .text_size(px(11.0))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_color(rgb(MAGENTA)).child("NEW PROJECT"))
                    .child(
                        div()
                            .id("cancel-project-x")
                            .cursor_pointer()
                            .text_color(rgb(MUTED))
                            .child("×")
                            .on_click(cx.listener(|form, _, _, cx| {
                                if !form.creating {
                                    cx.emit(ProjectCreationEvent::Cancelled);
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .items_end()
                    .child(div().flex_1().min_w_0().child(self.render_field(
                        Field::Path,
                        "FOLDER",
                        "/path/to/project",
                        cx,
                    )))
                    .child(
                        div()
                            .id("browse-project-folder")
                            .h(px(32.0))
                            .px(px(10.0))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .border_1()
                            .border_color(rgb(DIVIDER))
                            .text_color(rgb(CYAN))
                            .child("BROWSE…")
                            .on_click(cx.listener(|form, _, window, cx| form.browse(window, cx))),
                    ),
            )
            .child(self.render_field(
                Field::Name,
                if self.path_follows_name {
                    "PROJECT NAME"
                } else {
                    "NAME · OPTIONAL"
                },
                if self.path_follows_name {
                    "my-project"
                } else {
                    "Use folder name"
                },
                cx,
            ))
            .child(
                div()
                    .text_color(rgb(MUTED))
                    .text_size(px(10.0))
                    .child(summary),
            )
            .children(
                self.inspection
                    .as_ref()
                    .and_then(|value| value.warning.as_ref())
                    .map(|warning| div().text_color(rgb(GOLD)).child(warning.clone())),
            )
            .children(can_init.then(|| {
                div()
                    .id("init-project-git")
                    .flex()
                    .gap(px(8.0))
                    .items_center()
                    .cursor_pointer()
                    .text_color(rgb(CYAN))
                    .child(if self.init_git { "[✓]" } else { "[ ]" })
                    .child("Initialize Git  [CMD+G]")
                    .on_click(cx.listener(|form, _, _, cx| {
                        if !form.creating {
                            form.init_git = !form.init_git;
                            cx.notify();
                        }
                    }))
            }))
            .children(
                self.inspection
                    .as_ref()
                    .filter(|value| value.repository_count > 0)
                    .map(|_| {
                        div()
                            .text_color(rgb(MUTED))
                            .text_size(px(10.0))
                            .child("Use existing repositories")
                    }),
            )
            .children(
                self.error
                    .as_ref()
                    .map(|error| div().text_color(rgb(GOLD)).child(error.clone())),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(10.0))
                    .child(
                        div()
                            .id("cancel-project")
                            .px(px(12.0))
                            .py(px(8.0))
                            .cursor_pointer()
                            .text_color(rgb(MUTED))
                            .child("CANCEL")
                            .on_click(cx.listener(|form, _, _, cx| {
                                if !form.creating {
                                    cx.emit(ProjectCreationEvent::Cancelled);
                                }
                            })),
                    )
                    .child(
                        div()
                            .id("create-project")
                            .px(px(12.0))
                            .py(px(8.0))
                            .cursor_pointer()
                            .bg(rgb(PANEL_ACTIVE))
                            .border_1()
                            .border_color(rgb(if ready { CYAN } else { DIVIDER }))
                            .text_color(rgb(if ready { CYAN } else { MUTED }))
                            .child(if self.creating {
                                "CREATING…"
                            } else {
                                "CREATE PROJECT  ↵"
                            })
                            .on_click(cx.listener(|form, _, _, cx| form.submit(cx))),
                    ),
            )
    }
}

impl EntityInputHandler for ProjectCreator {
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
            input.text[..start].encode_utf16().count()..input.text[..end].encode_utf16().count(),
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
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
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
        self.replace(range, text, cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace(range, text, cx);
        if !text.is_empty() {
            let input = self.input_mut();
            let end = input.selection.end;
            let length = text.replace(['\n', '\r'], "").len();
            input.marked = (length > 0).then_some(end - length..end);
        }
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
    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.input().text.encode_utf16().count())
    }
    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        !self.creating
    }
}
