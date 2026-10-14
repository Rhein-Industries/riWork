//! A project's memo, at the top of its Project Settings: plain Markdown the person writes for
//! themselves (`riwork_core::project_memo`), saved by itself a moment after typing pauses, when
//! the editor loses focus, and when it goes away. A save replaces the whole memo and the last one
//! to reach the store wins, so a phone's save that lands after this window's is what both show
//! next; this editor takes a newer memo from the store only while it has nothing unsaved.

use std::{
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use gpui::{
    Context, Entity, EntityInputHandler, EventEmitter, Focusable, IntoElement, Render,
    Subscription, Task, Window, div, prelude::*, rgb,
};

use crate::{
    store::{State, Store},
    text_input::{self, EnterBehavior, InputEvent, TextareaState},
    theme, ui_text,
};
use riwork_core::project_memo::{self, ProjectMemo};

/// How long typing must pause before a save.
pub const SAVE_DELAY: Duration = Duration::from_millis(600);

pub const PLACEHOLDER: &str = "Write notes for this project in Markdown: plans, links, decisions. \
Only you see them. No agent is given the memo; type /memo in a chat to insert it yourself.";

/// What the line under the editor says about saving.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoStatus {
    /// The editor shows what the store has.
    Saved,
    /// Edited; a save follows when typing pauses.
    Pending,
    /// The store could not take the save. The text stays in the editor, and the next edit or
    /// Retry saves again.
    Failed(String),
    /// Longer than a memo may be: nothing is saved until it is shorter.
    TooLarge(String),
}

impl MemoStatus {
    pub fn message(&self) -> String {
        match self {
            Self::Saved => "Saved automatically".into(),
            Self::Pending => "Saving…".into(),
            Self::Failed(error) => format!("Not saved: {error}. Your text is still here."),
            Self::TooLarge(message) => message.clone(),
        }
    }
}

pub enum MemoEditorEvent {
    /// The editor took the keys: Project Settings' keyboard order is at the memo.
    Focused,
}

pub struct MemoEditor {
    store: Store,
    project_id: String,
    state: Entity<TextareaState>,
    /// The memo as this editor last read it from the store or saved it.
    synced: ProjectMemo,
    /// What the editor holds, as last typed or set.
    latest: String,
    status: MemoStatus,
    /// The save that waits for typing to pause.
    debounce: Option<Task<()>>,
    /// The save with the store. One at a time: one asked for meanwhile (`again`) follows it,
    /// with the text of that moment.
    in_flight: Option<Task<()>>,
    again: bool,
    /// The newest save written, shared by the background saves and `Drop`: a save older than
    /// one already written is skipped, so the last text typed is the last one written even when
    /// the store's lock makes an older save wait.
    written: Arc<Mutex<u64>>,
    generation: u64,
    _subscription: Subscription,
}

/// Write `text` as save `generation`, unless a newer one was written already (`None`).
fn write(
    store: &Store,
    project_id: &str,
    text: &str,
    written: &Mutex<u64>,
    generation: u64,
) -> Result<Option<ProjectMemo>, String> {
    let mut newest = written.lock().unwrap_or_else(PoisonError::into_inner);
    if *newest > generation {
        return Ok(None);
    }
    let (_, memo) = store.set_project_memo(project_id, text)?;
    *newest = generation;
    Ok(Some(memo))
}

impl EventEmitter<MemoEditorEvent> for MemoEditor {}

impl MemoEditor {
    pub fn new(
        store: Store,
        project_id: String,
        memo: ProjectMemo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = text_input::multiline(
            memo.text.clone(),
            PLACEHOLDER,
            8,
            28,
            EnterBehavior::Newline,
            window,
            cx,
        );
        let subscription =
            cx.subscribe_in(&state, window, |editor, state, event, _, cx| match event {
                InputEvent::Change => {
                    editor.latest = state.read(cx).value().to_string();
                    editor.edited(cx);
                }
                InputEvent::Focus => cx.emit(MemoEditorEvent::Focused),
                InputEvent::Blur => editor.save_now(cx),
                _ => {}
            });
        Self {
            store,
            project_id,
            state,
            latest: memo.text.clone(),
            synced: memo,
            status: MemoStatus::Saved,
            debounce: None,
            in_flight: None,
            again: false,
            written: Arc::new(Mutex::new(0)),
            generation: 0,
            _subscription: subscription,
        }
    }

    #[cfg(test)]
    pub fn status(&self) -> &MemoStatus {
        &self.status
    }

    #[cfg(test)]
    pub fn text(&self) -> &str {
        &self.latest
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// An input method is composing in the memo: Tab and Escape are its keys.
    pub fn is_composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.state.update(cx, |state, cx| {
            state.marked_text_range(window, cx).is_some()
        })
    }

    /// Whether what the editor holds differs from what the store last had.
    fn unsaved(&self) -> bool {
        project_memo::normalize(&self.latest) != self.synced.text
    }

    /// What the text says about saving, by itself: the same as the store's, too long, or due.
    fn status_now(&self) -> MemoStatus {
        let text = project_memo::normalize(&self.latest);
        if text == self.synced.text {
            MemoStatus::Saved
        } else if let Some(message) = project_memo::too_large(&text) {
            MemoStatus::TooLarge(message)
        } else {
            MemoStatus::Pending
        }
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.status = self.status_now();
        if self.in_flight.is_some() {
            self.again = true;
        } else if self.status == MemoStatus::Pending {
            self.save_after_pause(cx);
        } else {
            self.debounce = None;
        }
        cx.notify();
    }

    fn save_after_pause(&mut self, cx: &mut Context<Self>) {
        self.debounce = Some(cx.spawn(async move |editor, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let _ = editor.update(cx, |editor, cx| editor.save_now(cx));
        }));
    }

    /// Save what the editor holds now, if it differs from the store's and is not too long.
    pub fn save_now(&mut self, cx: &mut Context<Self>) {
        self.debounce = None;
        if self.in_flight.is_some() {
            self.again = true;
            return;
        }
        let text = project_memo::normalize(&self.latest).into_owned();
        if text == self.synced.text || project_memo::too_large(&text).is_some() {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        let (store, project_id) = (self.store.clone(), self.project_id.clone());
        let written = self.written.clone();
        let work = cx
            .background_executor()
            .spawn(async move { write(&store, &project_id, &text, &written, generation) });
        self.in_flight = Some(cx.spawn(async move |editor, cx| {
            let result = work.await;
            let _ = editor.update(cx, |editor, cx| editor.saved(result, cx));
        }));
    }

    fn saved(&mut self, result: Result<Option<ProjectMemo>, String>, cx: &mut Context<Self>) {
        self.in_flight = None;
        let again = std::mem::take(&mut self.again);
        match result {
            Ok(memo) => {
                if let Some(memo) = memo {
                    self.synced = memo;
                }
                self.status = self.status_now();
                // Typed while it was saved: that text is due like any edit.
                if self.status == MemoStatus::Pending {
                    self.save_after_pause(cx);
                }
            }
            Err(error) => {
                self.status = MemoStatus::Failed(error);
                // Typed while it failed: the newer text gets its own try.
                if again && self.status_now() == MemoStatus::Pending {
                    self.status = MemoStatus::Pending;
                    self.save_after_pause(cx);
                }
            }
        }
        cx.notify();
    }

    /// Try the last failed save again.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        self.status = self.status_now();
        self.save_now(cx);
        cx.notify();
    }

    /// Take the store's memo when someone else (a phone, the CLI, another window) saved a
    /// newer one and this editor has nothing unsaved. With unsaved text the editor keeps it,
    /// and its save, the later one, wins.
    pub fn refresh(&mut self, state: &State, window: &mut Window, cx: &mut Context<Self>) {
        let memo = state.project_memo(&self.project_id);
        if memo == self.synced {
            return;
        }
        if self.in_flight.is_some() || self.unsaved() {
            self.synced = memo;
            if self.in_flight.is_none() && !self.unsaved() {
                self.status = MemoStatus::Saved;
            }
            cx.notify();
            return;
        }
        self.state.update(cx, |state, cx| {
            state.set_value(memo.text.clone(), window, cx)
        });
        self.latest = memo.text.clone();
        self.synced = memo;
        self.status = MemoStatus::Saved;
        cx.notify();
    }
}

impl Render for MemoEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let warn = matches!(self.status, MemoStatus::Failed(_) | MemoStatus::TooLarge(_));
        let bytes = project_memo::normalize(&self.latest).len();
        div()
            .flex()
            .flex_col()
            .gap(ui_text::space(6.0))
            .child(
                text_input::textarea("project-memo-editor", &self.state, window, cx)
                    .font_family(ui_text::mono_family()),
            )
            .child(
                div()
                    .id("project-memo-status")
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(ui_text::space(8.0))
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(if warn { colors.magenta } else { colors.muted }))
                    .child(self.status.message())
                    .when(bytes * 5 >= project_memo::MAX_BYTES * 4, |row| {
                        row.child(format!("{bytes} / {} bytes", project_memo::MAX_BYTES))
                    })
                    .when(matches!(self.status, MemoStatus::Failed(_)), |row| {
                        row.child(
                            div()
                                .id("project-memo-retry")
                                .cursor_pointer()
                                .text_color(rgb(colors.cyan))
                                .child(ui_text::cased("Retry"))
                                .on_click(cx.listener(|editor, _, _, cx| editor.retry(cx))),
                        )
                    }),
            )
    }
}

impl Drop for MemoEditor {
    /// Closing Project Settings or switching the window's project must not lose what was typed
    /// in the last moment: it is saved here, at once, unless it is too long to be saved.
    /// A save still with the store is waited for, and this one, newer, is written after it.
    fn drop(&mut self) {
        let text = project_memo::normalize(&self.latest);
        if text != self.synced.text
            && project_memo::too_large(&text).is_none()
            && let Err(error) = write(
                &self.store,
                &self.project_id,
                &text,
                &self.written,
                self.generation + 1,
            )
        {
            eprintln!("riwork: the project memo could not be saved: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::form_input::{test_turn, test_window};
    use gpui::TestAppContext;

    struct Fixture {
        dir: std::path::PathBuf,
        store: Store,
        project: String,
        other: String,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("riwork-memo-editor-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(dir.join("a")).unwrap();
            std::fs::create_dir_all(dir.join("b")).unwrap();
            let store = Store::open(dir.join("state")).unwrap();
            let project = store.add_project(dir.join("a"), Some("A")).unwrap().id;
            let other = store.add_project(dir.join("b"), Some("B")).unwrap().id;
            Self {
                dir,
                store,
                project,
                other,
            }
        }

        fn saved(&self, id: &str) -> ProjectMemo {
            self.store.snapshot().unwrap().project_memo(id)
        }

        fn open(&self, cx: &mut TestAppContext) -> (gpui::AnyWindowHandle, Entity<MemoEditor>) {
            let (store, id) = (self.store.clone(), self.project.clone());
            let memo = self.saved(&id);
            test_window(cx, move |window, cx| {
                MemoEditor::new(store, id, memo, window, cx)
            })
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // Made by this test for itself.
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn type_text(cx: &mut TestAppContext, handle: gpui::AnyWindowHandle, text: &'static str) {
        use gpui_kit::test::TestWindowExt as _;
        test_turn(cx, handle, move |window, app| {
            window.click("project-memo-editor", app);
            window.input(text, app);
        });
    }

    #[gpui::test]
    fn typing_saves_after_a_pause_and_only_this_projects_memo(cx: &mut TestAppContext) {
        let f = Fixture::new();
        f.store.set_project_memo(&f.other, "other notes").unwrap();
        let other = f.saved(&f.other);
        let (handle, editor) = f.open(cx);
        editor.read_with(cx, |editor, _| {
            assert_eq!(editor.status(), &MemoStatus::Saved);
            assert_eq!(editor.text(), "");
        });

        type_text(cx, handle, "# Plan");
        editor.read_with(cx, |editor, _| {
            assert_eq!(editor.status(), &MemoStatus::Pending);
            assert_eq!(editor.status().message(), "Saving…");
        });
        // Nothing is written while typing goes on.
        assert!(f.saved(&f.project).is_empty());

        cx.executor().advance_clock(SAVE_DELAY);
        cx.run_until_parked();
        assert_eq!(f.saved(&f.project).text, "# Plan");
        editor.read_with(cx, |editor, _| {
            assert_eq!(editor.status(), &MemoStatus::Saved);
            assert_eq!(editor.status().message(), "Saved automatically");
        });
        assert_eq!(f.saved(&f.other), other);
    }

    #[gpui::test]
    fn a_newer_memo_from_elsewhere_replaces_only_an_unedited_editor(cx: &mut TestAppContext) {
        let f = Fixture::new();
        let (handle, editor) = f.open(cx);

        // The phone saved; this editor had nothing unsaved and shows the phone's memo.
        f.store
            .set_project_memo(&f.project, "from the phone")
            .unwrap();
        let state = f.store.snapshot().unwrap();
        test_turn(cx, handle, |window, app| {
            editor.update(app, |editor, cx| editor.refresh(&state, window, cx));
        });
        editor.read_with(cx, |editor, _| {
            assert_eq!(editor.text(), "from the phone");
            assert_eq!(editor.status(), &MemoStatus::Saved);
        });

        // Typing here, then another phone save: the typed text stays and its save wins.
        type_text(cx, handle, " and the desktop");
        f.store.set_project_memo(&f.project, "phone again").unwrap();
        let state = f.store.snapshot().unwrap();
        test_turn(cx, handle, |window, app| {
            editor.update(app, |editor, cx| editor.refresh(&state, window, cx));
        });
        editor.read_with(cx, |editor, _| {
            assert!(
                editor.text().ends_with(" and the desktop"),
                "{}",
                editor.text()
            );
            assert_eq!(editor.status(), &MemoStatus::Pending);
        });
        cx.executor().advance_clock(SAVE_DELAY);
        cx.run_until_parked();
        assert!(f.saved(&f.project).text.ends_with(" and the desktop"));
    }

    #[gpui::test]
    fn too_long_is_said_and_never_saved_and_closing_saves_the_rest(cx: &mut TestAppContext) {
        let f = Fixture::new();
        f.store.set_project_memo(&f.project, "kept").unwrap();
        let (handle, editor) = f.open(cx);
        let long = "x".repeat(project_memo::MAX_BYTES + 1);
        test_turn(cx, handle, |window, app| {
            editor.update(app, |editor, cx| {
                editor
                    .state
                    .update(cx, |state, cx| state.replace_all(long.clone(), window, cx))
            });
        });
        cx.executor().advance_clock(SAVE_DELAY * 2);
        cx.run_until_parked();
        editor.read_with(cx, |editor, _| match editor.status() {
            MemoStatus::TooLarge(message) => assert!(message.contains("32768"), "{message}"),
            other => panic!("{other:?}"),
        });
        assert_eq!(f.saved(&f.project).text, "kept");

        // Shortened and closed before the pause: the window's going saves it at once.
        test_turn(cx, handle, |window, app| {
            editor.update(app, |editor, cx| {
                editor
                    .state
                    .update(cx, |state, cx| state.replace_all("last words", window, cx))
            });
        });
        let weak = editor.downgrade();
        drop(editor);
        cx.update_window(handle, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        cx.update(|_| {});
        assert!(weak.upgrade().is_none(), "the editor went with its window");
        assert_eq!(f.saved(&f.project).text, "last words");
    }

    /// Typing during a save, even back to what the store had before it, is saved after it.
    #[gpui::test]
    fn an_edit_during_a_save_is_saved_after_it(cx: &mut TestAppContext) {
        let f = Fixture::new();
        f.store.set_project_memo(&f.project, "A").unwrap();
        let (handle, editor) = f.open(cx);
        let set = |editor: &Entity<MemoEditor>,
                   text: &'static str,
                   window: &mut Window,
                   app: &mut gpui::App| {
            editor.update(app, |editor, cx| {
                editor
                    .state
                    .update(cx, |state, cx| state.replace_all(text, window, cx))
            });
        };
        test_turn(cx, handle, |window, app| set(&editor, "AB", window, app));
        test_turn(cx, handle, |window, app| {
            // The save of "AB" starts; then the text goes back to "A" while it is with the store.
            editor.update(app, |editor, cx| editor.save_now(cx));
            set(&editor, "A", window, app);
        });
        editor.read_with(cx, |editor, _| {
            assert!(editor.in_flight.is_none(), "the save of AB came back");
            assert_eq!(editor.status(), &MemoStatus::Pending, "A is not saved yet");
        });
        assert_eq!(f.saved(&f.project).text, "AB");
        cx.executor().advance_clock(SAVE_DELAY);
        cx.run_until_parked();
        assert_eq!(f.saved(&f.project).text, "A");
        editor.read_with(cx, |editor, _| {
            assert_eq!(editor.status(), &MemoStatus::Saved)
        });
    }

    /// A save older than one already written is skipped: the last text is the last written.
    #[test]
    fn an_older_save_never_lands_after_a_newer_one() {
        let f = Fixture::new();
        let written = Mutex::new(0);
        assert!(
            write(&f.store, &f.project, "newer", &written, 2)
                .unwrap()
                .is_some()
        );
        assert!(
            write(&f.store, &f.project, "older", &written, 1)
                .unwrap()
                .is_none()
        );
        assert_eq!(f.saved(&f.project).text, "newer");
        assert!(
            write(&f.store, &f.project, "newest", &written, 3)
                .unwrap()
                .is_some()
        );
        assert_eq!(f.saved(&f.project).text, "newest");
    }

    #[gpui::test]
    fn a_failed_save_keeps_the_text_and_says_so(cx: &mut TestAppContext) {
        let f = Fixture::new();
        let (handle, editor) = f.open(cx);
        // A project that no longer resolves: the store refuses.
        editor.update(cx, |editor, _| editor.project_id = "missing".into());
        type_text(cx, handle, "precious");
        cx.executor().advance_clock(SAVE_DELAY);
        cx.run_until_parked();
        editor.read_with(cx, |editor, _| {
            assert!(matches!(editor.status(), MemoStatus::Failed(_)));
            assert!(
                editor
                    .status()
                    .message()
                    .contains("Your text is still here")
            );
            assert_eq!(editor.text(), "precious");
        });
        editor.update(cx, |editor, cx| {
            editor.project_id = f.project.clone();
            editor.retry(cx);
        });
        cx.run_until_parked();
        assert_eq!(f.saved(&f.project).text, "precious");
        editor.read_with(cx, |editor, _| {
            assert_eq!(editor.status(), &MemoStatus::Saved)
        });
    }
}
