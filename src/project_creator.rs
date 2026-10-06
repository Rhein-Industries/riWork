//! Compact project creation with optional Git initialization.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::text_input::{self, EnterBehavior, InputEvent, InputState};
use gpui::{
    AnyElement, Context, EventEmitter, FocusHandle, IntoElement, KeyDownEvent, Modifiers,
    MouseButton, PathPromptOptions, Render, Window, div, prelude::*, rgb,
};
use gpui::{Entity, Focusable, Subscription};
#[cfg(test)]
use std::ops::Range;

use crate::{
    behavior_controls as behavior,
    store::{Project, ProjectInspection, Store},
    theme, ui_text,
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

#[cfg(test)]
#[derive(Default)]
struct Input {
    text: String,
    selection: Range<usize>,
}

/// Expands `~` and requires an absolute folder. A bare or relative path would
/// resolve against the process directory, which is `/` when Finder launches the app.
fn resolve_folder(text: &str, home: Option<&OsStr>) -> Result<PathBuf, String> {
    let text = text.trim();
    let expand = |tail: &str| {
        let home = home
            .filter(|home| !home.is_empty())
            .ok_or("Cannot expand ~: HOME is not set. Enter an absolute folder path.")?;
        Ok::<_, String>(PathBuf::from(home).join(tail))
    };
    let path = if text == "~" {
        expand("")?
    } else if let Some(tail) = text.strip_prefix("~/") {
        expand(tail)?
    } else {
        PathBuf::from(text)
    };
    if !path.is_absolute() {
        return Err("Use an absolute folder path, starting with / or ~/".to_owned());
    }
    if path
        .components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case(".git"))
    {
        return Err("A project folder cannot be named .git or live inside it".to_owned());
    }
    Ok(path)
}

/// The text a copy or cut would place on the clipboard; an empty selection must
/// leave the clipboard alone.
#[cfg(test)]
fn selected_text(input: &Input) -> Option<String> {
    let text = &input.text[input.selection.clone()];
    (!text.is_empty()).then(|| text.to_owned())
}

/// Cmd+G toggles Git initialization. With Shift it is the global Open Grok shortcut.
fn init_git_shortcut(modifiers: &Modifiers) -> bool {
    modifiers.platform && !modifiers.shift
}

/// The directories creating `root` would have to make, deepest first.
fn missing_directories(root: &Path) -> Vec<PathBuf> {
    root.ancestors()
        .take_while(|directory| {
            matches!(
                fs::symlink_metadata(directory),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            )
        })
        .map(Path::to_path_buf)
        .collect()
}

/// Removes directories a failed creation made. `remove_dir` only takes empty
/// ones, so anything left inside, such as a `.git` from a completed `git init`,
/// keeps its directory and every parent above it.
fn remove_created_directories(created: &[PathBuf]) {
    for directory in created {
        if fs::remove_dir(directory).is_err() {
            break;
        }
    }
}

fn create_project_cleaning_up(
    store: &Store,
    path: &Path,
    name: Option<&str>,
    init_git: bool,
) -> Result<Project, String> {
    let created = missing_directories(path);
    store
        .create_project(path, name, init_git)
        .inspect_err(|_| remove_created_directories(&created))
}

pub struct ProjectCreator {
    path: String,
    path_state: Entity<InputState>,
    name_state: Entity<InputState>,
    _input_subscriptions: Vec<Subscription>,
    name: String,
    default_directory: PathBuf,
    path_follows_name: bool,
    active: Field,
    focus: FocusHandle,
    dialog: gpui_kit::base::DialogHandle,
    return_focus: Option<FocusHandle>,
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
        let return_focus = window.focused(cx);
        cx.observe_global::<theme::Appearance>(|_, cx| cx.notify())
            .detach();
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let path = format!("{}/", default_directory.display());

        let path_state = text_input::single_line(path.clone(), "/path/to/project", window, cx);
        let name_state = text_input::single_line("", "my-project", window, cx);
        let mut subscriptions = Vec::new();
        for (field, state) in [(Field::Path, &path_state), (Field::Name, &name_state)] {
            subscriptions.push(cx.subscribe_in(
                state,
                window,
                move |form, state, event, window, cx| {
                    if form.creating {
                        return;
                    }
                    match event {
                        InputEvent::Change => {
                            let value = state.read(cx).value().to_string();
                            match field {
                                Field::Path => {
                                    form.path = value;
                                    form.path_follows_name = false;
                                    form.name_state.update(cx, |state, cx| {
                                        state.set_placeholder("Use folder name", window, cx)
                                    });
                                }
                                Field::Name => form.name = value,
                            }
                            form.inspect(window, cx);
                        }
                        InputEvent::Focus => {
                            form.active = field;
                            cx.notify();
                        }
                        _ if text_input::is_submit(event, EnterBehavior::Submit) => form.submit(cx),
                        _ => {}
                    }
                },
            ));
        }
        name_state.read(cx).focus_handle(cx).focus(window, cx);
        Self {
            path_state,
            name_state,
            _input_subscriptions: subscriptions,
            path,
            name: String::new(),
            default_directory,
            path_follows_name: true,
            active: Field::Name,
            focus,
            dialog: gpui_kit::base::DialogHandle::new(true),
            return_focus,
            store,
            inspection: None,
            generation: 0,
            inspecting: false,
            creating: false,
            init_git: true,
            error: None,
        }
    }

    fn path_value(&self) -> Result<PathBuf, String> {
        resolve_folder(&self.path, std::env::var_os("HOME").as_deref())
    }

    fn inspect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        self.inspection = None;
        self.error = None;
        if self.path_follows_name {
            let name = self.name.trim();
            let path = if name.is_empty() {
                Ok(self.default_directory.clone())
            } else {
                crate::paths::default_new_project_path(name)
            };
            match path {
                Ok(path) => {
                    self.path = path.to_string_lossy().into_owned();
                    if name.is_empty() {
                        self.path.push('/');
                    }

                    crate::form_input::set_value(&self.path_state, self.path.clone(), window, cx);
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
        if self.path.trim().is_empty() {
            self.inspecting = false;
            cx.notify();
            return;
        }
        let path = match self.path_value() {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(error);
                self.inspecting = false;
                cx.notify();
                return;
            }
        };
        self.inspecting = true;
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
                    form.name_state.update(cx, |state, cx| {
                        state.set_placeholder("Use folder name", window, cx)
                    });
                    form.path = path.to_string_lossy().into_owned();

                    form.active = Field::Path;
                    form.path_state.read(cx).focus_handle(cx).focus(window, cx);
                    form.path_state.update(cx, |state, cx| {
                        state.set_value(form.path.clone(), window, cx)
                    });
                    form.inspect(window, cx);
                });
            }
        })
        .detach();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        self.name = self.name_state.read(cx).value().to_string();
        self.path = self.path_state.read(cx).value().to_string();
        if self.creating || self.inspecting || self.inspection.is_none() {
            return;
        }
        let path = match self.path_value() {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let name = self.name.trim().to_owned();
        let init_git = self.init_git
            && self
                .inspection
                .as_ref()
                .is_some_and(|value| value.can_init_git);
        let store = self.store.clone();
        self.creating = true;
        self.error = None;
        let work = cx.background_executor().spawn(async move {
            create_project_cleaning_up(
                &store,
                &path,
                (!name.is_empty()).then_some(name.as_str()),
                init_git,
            )
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

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let state = match self.active {
            Field::Path => &self.path_state,
            Field::Name => &self.name_state,
        };
        if matches!(event.keystroke.key.as_str(), "escape" | "tab")
            && crate::form_input::is_composing(state, window, cx)
        {
            return;
        }
        if self.creating {
            cx.stop_propagation();
            return;
        }
        let platform = event.keystroke.modifiers.platform;
        let handled = match event.keystroke.key.as_str() {
            "escape" => {
                crate::project_settings::close_modal(
                    &self.dialog,
                    &self.focus,
                    &self.return_focus,
                    window,
                    cx,
                );
                cx.emit(ProjectCreationEvent::Cancelled);
                true
            }

            "tab" => {
                crate::project_settings::modal_tab(event.keystroke.modifiers.shift, window, cx);
                true
            }
            "g" if init_git_shortcut(&event.keystroke.modifiers) => {
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
            _ => false,
        };
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        let state = match self.active {
            Field::Path => &self.path_state,
            Field::Name => &self.name_state,
        };
        state.read(cx).focus_handle(cx).focus(window, cx);
    }

    fn render_field(
        &self,
        field: Field,
        label: &str,
        _placeholder: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let (state, id) = match field {
            Field::Path => (&self.path_state, "project-path"),
            Field::Name => (&self.name_state, "project-name"),
        };
        div()
            .flex()
            .flex_col()
            .gap(ui_text::space(5.0))
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .text_size(ui_text::text(9.0))
                    .child(ui_text::cased(label.to_owned())),
            )
            .child(crate::form_input::plain_frame(
                id,
                state,
                self.creating,
                window,
                cx,
            ))
            .into_any_element()
    }
}

impl Render for ProjectCreator {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let ready = !self.creating && !self.inspecting && self.inspection.is_some();
        let can_init = self
            .inspection
            .as_ref()
            .is_some_and(|value| value.can_init_git);
        let height = 288.0
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
        let panel = div()
            .id("project-creator")
            // Pinned Base Dialog's focus-trap host omits its AX role.
            .role(gpui::Role::Dialog)
            .aria_label("New project")
            .occlude()
            .key_context("ProjectCreator")
            .capture_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .w(ui_text::space(540.0))
            .h(ui_text::space(height))
            .flex_none()
            .max_w_full()
            .max_h(gpui::relative(0.9))
            .overflow_y_scroll()
            .p(ui_text::space(16.0))
            .flex()
            .flex_col()
            .gap(ui_text::space(14.0))
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.magenta))
            .text_size(ui_text::text(11.0))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_color(rgb(colors.magenta))
                            .child(ui_text::cased("New project")),
                    )
                    .child(
                        behavior::button_content("cancel-project-x", "Close new project", "×")
                            .disabled(self.creating)
                            .focus_visible(move |style| style.text_color(rgb(colors.focus)))
                            .text_color(rgb(colors.muted))
                            .on_click(cx.listener(|form, _, window, cx| {
                                if !form.creating {
                                    crate::project_settings::close_modal(
                                        &form.dialog,
                                        &form.focus,
                                        &form.return_focus,
                                        window,
                                        cx,
                                    );
                                    cx.emit(ProjectCreationEvent::Cancelled);
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(ui_text::space(8.0))
                    .items_end()
                    .child(div().flex_1().min_w_0().child(self.render_field(
                        Field::Path,
                        "Folder",
                        "/path/to/project",
                        window,
                        cx,
                    )))
                    .child(
                        behavior::button_content(
                            "browse-project-folder",
                            "Browse project folder",
                            ui_text::cased("Browse…"),
                        )
                        .disabled(self.creating)
                        .focus_visible(move |style| style.border_color(rgb(colors.focus)))
                        .h(ui_text::space(32.0))
                        .px(ui_text::space(10.0))
                        .flex()
                        .items_center()
                        .border_1()
                        .border_color(rgb(colors.divider))
                        .text_color(rgb(colors.cyan))
                        .on_click(cx.listener(|form, _, window, cx| form.browse(window, cx))),
                    ),
            )
            .child(self.render_field(
                Field::Name,
                if self.path_follows_name {
                    "Project name"
                } else {
                    "Name · Optional"
                },
                if self.path_follows_name {
                    "my-project"
                } else {
                    "Use folder name"
                },
                window,
                cx,
            ))
            .child(
                div()
                    .text_color(rgb(colors.muted))
                    .text_size(ui_text::text(10.0))
                    .child(summary),
            )
            .child(
                // The folder the project will use once `~` and symlinks are resolved.
                div()
                    .h(ui_text::space(14.0))
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(rgb(colors.muted))
                    .text_size(ui_text::text(10.0))
                    .child(
                        self.inspection
                            .as_ref()
                            .map(|value| format!("Folder: {}", value.root.display()))
                            .unwrap_or_default(),
                    ),
            )
            .children(
                self.inspection
                    .as_ref()
                    .and_then(|value| value.warning.as_ref())
                    .map(|warning| div().text_color(rgb(colors.gold)).child(warning.clone())),
            )
            .children(can_init.then(|| {
                behavior::switch_content(
                    "init-project-git",
                    "Initialize Git",
                    if self.init_git { "[✓]" } else { "[ ]" },
                    self.init_git,
                )
                .disabled(self.creating)
                .flex()
                .gap(ui_text::space(8.0))
                .items_center()
                .text_color(rgb(colors.cyan))
                .child("Initialize Git  [CMD+G]")
                .on_change({
                    let listener = cx.listener(|form, checked: &bool, _, cx| {
                        if !form.creating {
                            form.init_git = *checked;
                            cx.notify();
                        }
                    });
                    move |checked, _, window, cx| listener(&checked, window, cx)
                })
            }))
            .children(
                self.inspection
                    .as_ref()
                    .filter(|value| value.repository_count > 0)
                    .map(|_| {
                        div()
                            .text_color(rgb(colors.muted))
                            .text_size(ui_text::text(10.0))
                            .child("Use existing repositories")
                    }),
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
                            "cancel-project",
                            "Cancel",
                            ui_text::cased("Cancel"),
                        )
                        .disabled(self.creating)
                        .focus_visible(move |style| {
                            style
                                .text_color(rgb(colors.focus))
                                .border_color(rgb(colors.focus))
                        })
                        .px(ui_text::space(12.0))
                        .py(ui_text::space(8.0))
                        .text_color(rgb(colors.muted))
                        .on_click(cx.listener(|form, _, window, cx| {
                            if !form.creating {
                                crate::project_settings::close_modal(
                                    &form.dialog,
                                    &form.focus,
                                    &form.return_focus,
                                    window,
                                    cx,
                                );
                                cx.emit(ProjectCreationEvent::Cancelled);
                            }
                        })),
                    )
                    .child(
                        behavior::button_content(
                            "create-project",
                            "Create project",
                            ui_text::cased(if self.creating {
                                "Creating…"
                            } else {
                                "Create project  ↵"
                            }),
                        )
                        .disabled(self.creating)
                        .focus_visible(move |style| {
                            style
                                .text_color(rgb(colors.focus))
                                .border_color(rgb(colors.focus))
                        })
                        .px(ui_text::space(12.0))
                        .py(ui_text::space(8.0))
                        .bg(rgb(colors.panel_active))
                        .border_1()
                        .border_color(rgb(if ready { colors.cyan } else { colors.divider }))
                        .text_color(rgb(if ready { colors.cyan } else { colors.muted }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("riwork-creator-test-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        /// A store whose state file cannot be parsed: creating a project makes
        /// its directories and then fails to register it.
        fn failing_store(&self) -> Store {
            let state = self.0.join("state");
            let store = Store::open(&state).unwrap();
            fs::write(state.join("state.json"), "not json").unwrap();
            store
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn folders_are_absolute_and_expand_the_home_shorthand() {
        let home = OsStr::new("/Users/someone");
        let resolve = |text: &str| resolve_folder(text, Some(home));
        assert_eq!(resolve("~").unwrap(), Path::new("/Users/someone"));
        assert_eq!(
            resolve(" ~/work/app ").unwrap(),
            Path::new("/Users/someone/work/app")
        );
        assert_eq!(resolve("/srv/app").unwrap(), Path::new("/srv/app"));
        for relative in ["app", "./app", "../app", "~someone/app", "~app", ""] {
            assert!(
                resolve(relative).unwrap_err().contains("absolute"),
                "{relative:?}"
            );
        }
        assert!(resolve_folder("~/app", None).unwrap_err().contains("HOME"));
        assert!(
            resolve_folder("~", Some(OsStr::new("")))
                .unwrap_err()
                .contains("HOME")
        );
        assert!(resolve_folder("~/app", Some(OsStr::new("relative-home"))).is_err());
    }

    #[test]
    fn a_folder_cannot_be_git_metadata() {
        let home = Some(OsStr::new("/Users/someone"));
        for text in [
            "~/Documents/riwork/.git",
            "/srv/.GIT",
            "/srv/.Git/hooks",
            "~/.git/app",
        ] {
            assert!(
                resolve_folder(text, home).unwrap_err().contains(".git"),
                "{text}"
            );
        }
        assert!(resolve_folder("/srv/.github", home).is_ok());
        assert!(resolve_folder("/srv/my.git", home).is_ok());
    }

    #[test]
    fn copy_and_cut_need_a_selection() {
        let mut input = Input {
            text: "/path/to/app".to_owned(),
            selection: 3..3,
            ..Default::default()
        };
        assert_eq!(selected_text(&input), None);
        input.selection = 1..5;
        assert_eq!(selected_text(&input).as_deref(), Some("path"));
        assert_eq!(selected_text(&Input::default()), None);
    }

    #[test]
    fn shift_leaves_cmd_g_to_the_global_open_grok_shortcut() {
        let command = Modifiers {
            platform: true,
            ..Default::default()
        };
        assert!(init_git_shortcut(&command));
        assert!(!init_git_shortcut(&Modifiers {
            shift: true,
            ..command
        }));
        assert!(!init_git_shortcut(&Modifiers::default()));
    }

    #[test]
    fn failed_creation_removes_the_directories_it_made() {
        let fixture = Fixture::new();
        let store = fixture.failing_store();
        let root = fixture.0.join("a/b/c");
        assert!(create_project_cleaning_up(&store, &root, None, false).is_err());
        assert!(!fixture.0.join("a").exists());
        assert!(fixture.0.join("state").is_dir());
    }

    #[test]
    fn failed_creation_keeps_directories_that_already_existed() {
        let fixture = Fixture::new();
        let store = fixture.failing_store();
        fs::create_dir_all(fixture.0.join("a")).unwrap();
        fs::write(fixture.0.join("a/keep.txt"), "mine").unwrap();
        let root = fixture.0.join("a/b/c");
        assert!(create_project_cleaning_up(&store, &root, None, false).is_err());
        assert!(fixture.0.join("a/keep.txt").is_file());
        assert!(!fixture.0.join("a/b").exists());
        // An existing empty folder is not ours to remove either.
        let existing = fixture.0.join("empty");
        fs::create_dir_all(&existing).unwrap();
        assert!(create_project_cleaning_up(&store, &existing, None, false).is_err());
        assert!(existing.is_dir());
    }

    #[test]
    fn failed_creation_keeps_a_directory_git_init_filled() {
        let fixture = Fixture::new();
        let store = fixture.failing_store();
        let root = fixture.0.join("a/b");
        assert!(create_project_cleaning_up(&store, &root, None, true).is_err());
        // Registration failed after `git init`, so `.git` and its parents stay.
        assert!(root.join(".git").is_dir());
    }

    #[test]
    fn successful_creation_keeps_its_directories() {
        let fixture = Fixture::new();
        let store = Store::open(fixture.0.join("state")).unwrap();
        let root = fixture.0.join("a/b");
        let project = create_project_cleaning_up(&store, &root, Some("App"), false).unwrap();
        assert_eq!(project.root, root);
        assert!(root.is_dir());
    }
}
