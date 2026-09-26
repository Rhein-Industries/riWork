//! Saved application preferences, shared by every RiWork window and process.

use std::{
    env, fs,
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
};

use fs2::FileExt;
use gpui::{
    AnyElement, Context, FocusHandle, Global, IntoElement, KeyDownEvent, MouseButton, Render,
    Window, div, prelude::*, px, rgb,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{BG, CYAN, DIVIDER, MUTED, PANEL, PANEL_ACTIVE, TEXT};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub schema_version: u32,
    pub use_riwork_colors: bool,
    pub remember_window_size: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            use_riwork_colors: false,
            remember_window_size: true,
        }
    }
}

impl Global for Settings {}

#[derive(Clone)]
pub struct SettingsStore {
    dir: PathBuf,
}

impl SettingsStore {
    pub fn open_default() -> Result<Self, String> {
        let dir = match env::var_os("RIWORK_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(env::var_os("HOME").ok_or("HOME is unset; set RIWORK_HOME")?)
                .join(".local/share/riwork"),
        };
        Self::open(dir)
    }

    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, String> {
        let dir = dir.into();
        fs::create_dir_all(&dir)
            .map_err(|error| format!("Cannot create settings directory: {error}"))?;
        Ok(Self { dir })
    }

    pub fn load(&self) -> Result<Settings, String> {
        let lock = self.lock_file()?;
        FileExt::lock_shared(&lock).map_err(|error| format!("Cannot lock settings: {error}"))?;
        self.read()
    }

    pub fn update(&self, change: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
        let lock = self.lock_file()?;
        FileExt::lock_exclusive(&lock).map_err(|error| format!("Cannot lock settings: {error}"))?;
        let mut settings = self.read()?;
        change(&mut settings);
        let path = self.dir.join("settings.json");
        let temporary = self.dir.join(format!(".settings-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| format!("Cannot create settings: {error}"))?;
            serde_json::to_writer_pretty(&mut file, &settings)
                .map_err(|error| format!("Cannot encode settings: {error}"))?;
            file.write_all(b"\n")
                .and_then(|_| file.sync_all())
                .map_err(|error| format!("Cannot save settings: {error}"))?;
            fs::rename(&temporary, &path)
                .map_err(|error| format!("Cannot replace settings: {error}"))?;
            File::open(&self.dir)
                .and_then(|dir| dir.sync_all())
                .map_err(|error| format!("Cannot sync settings directory: {error}"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result?;
        Ok(settings)
    }

    fn lock_file(&self) -> Result<File, String> {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join("settings.lock"))
            .map_err(|error| format!("Cannot open settings lock: {error}"))
    }

    fn read(&self) -> Result<Settings, String> {
        let path = self.dir.join("settings.json");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Settings::default());
            }
            Err(error) => return Err(format!("Cannot read settings: {error}")),
        };
        let settings: Settings = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Cannot parse settings: {error}"))?;
        if settings.schema_version != 1 {
            return Err(format!(
                "Unsupported settings schema {}",
                settings.schema_version
            ));
        }
        Ok(settings)
    }
}

pub struct SettingsPanel {
    store: SettingsStore,
    theme_focus: FocusHandle,
    size_focus: FocusHandle,
    error: Option<String>,
}

impl SettingsPanel {
    pub fn new(store: SettingsStore, cx: &mut Context<Self>) -> Self {
        let theme_focus = cx.focus_handle();
        cx.observe_global::<Settings>(|_, cx| cx.notify()).detach();
        Self {
            store,
            theme_focus,
            size_focus: cx.focus_handle(),
            error: None,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.size_focus.is_focused(window) {
            self.theme_focus.focus(window, cx);
        }
    }

    fn change(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        match self.store.update(change) {
            Ok(settings) => {
                cx.set_global(settings);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.platform || event.keystroke.modifiers.control {
            return;
        }
        match event.keystroke.key.as_str() {
            "tab" => {
                if self.theme_focus.is_focused(window) {
                    self.size_focus.focus(window, cx);
                } else {
                    self.theme_focus.focus(window, cx);
                }
            }
            "space" | "enter" | "return" => {
                if self.theme_focus.is_focused(window) {
                    self.change(
                        |settings| settings.use_riwork_colors = !settings.use_riwork_colors,
                        cx,
                    );
                } else {
                    self.change(
                        |settings| settings.remember_window_size = !settings.remember_window_size,
                        cx,
                    );
                }
            }
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    fn row(
        &self,
        title: &'static str,
        description: &'static str,
        enabled: bool,
        theme: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focus = if theme {
            &self.theme_focus
        } else {
            &self.size_focus
        };
        div()
            .id(if theme {
                "terminal-colors"
            } else {
                "remember-window-size"
            })
            .track_focus(focus)
            .flex()
            .items_center()
            .gap(px(16.0))
            .p(px(12.0))
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(DIVIDER))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(PANEL_ACTIVE)))
            .focus_visible(|style| style.border_color(rgb(CYAN)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(div().text_size(px(14.0)).text_color(rgb(TEXT)).child(title))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(MUTED))
                            .child(description),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(42.0))
                    .py(px(5.0))
                    .rounded(px(4.0))
                    .bg(rgb(if enabled { 0x174b49 } else { PANEL_ACTIVE }))
                    .text_color(rgb(if enabled { CYAN } else { MUTED }))
                    .text_size(px(11.0))
                    .text_center()
                    .child(if enabled { "ON" } else { "OFF" }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |settings, _, window, cx| {
                    if theme {
                        &settings.theme_focus
                    } else {
                        &settings.size_focus
                    }
                    .focus(window, cx);
                }),
            )
            .on_click(cx.listener(move |view, _, _, cx| {
                view.change(
                    |settings| {
                        if theme {
                            settings.use_riwork_colors = !settings.use_riwork_colors;
                        } else {
                            settings.remember_window_size = !settings.remember_window_size;
                        }
                    },
                    cx,
                );
            }))
            .into_any_element()
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx.global::<Settings>().clone();
        div()
            .id("settings-panel")
            .key_context("RiWorkSettings")
            .on_key_down(cx.listener(Self::key_down))
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(BG))
            .p(px(16.0))
            .child(div()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(620.0))
            .gap(px(14.0))
            .font_family("Menlo")
            .text_color(rgb(TEXT))
            .child(div().text_size(px(22.0)).child("Settings"))
            .child(div().mt(px(4.0)).text_size(px(12.0)).text_color(rgb(MUTED)).child("TERMINAL"))
            .child(self.row(
                "Use RiWork terminal colors",
                "Off uses your Ghostty configuration and colors. Changes apply to all open terminals.",
                settings.use_riwork_colors,
                true,
                cx,
            ))
            .child(div().mt(px(4.0)).text_size(px(12.0)).text_color(rgb(MUTED)).child("WINDOWS"))
            .child(self.row(
                "Remember project window size",
                "Open each project at its last window size. Switching projects keeps this window's size.",
                settings.remember_window_size,
                false,
                cx,
            ))
            .children(self.error.as_ref().map(|error| div().text_size(px(12.0)).text_color(rgb(crate::GOLD)).child(error.clone())))
            .child(div().text_size(px(11.0)).text_color(rgb(MUTED)).child("Saved automatically · Tab to move · Space to toggle")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_older_settings_preserve_terminal_colors_by_default() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.use_riwork_colors);
        assert!(settings.remember_window_size);
    }

    #[test]
    fn concurrent_preferences_merge_and_corrupt_data_is_not_overwritten() {
        let dir = env::temp_dir().join(format!("riwork-settings-test-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        assert_eq!(store.load().unwrap(), Settings::default());
        let other = store.clone();
        let worker = std::thread::spawn(move || {
            other
                .update(|settings| settings.use_riwork_colors = true)
                .unwrap()
        });
        store
            .update(|settings| settings.remember_window_size = false)
            .unwrap();
        worker.join().unwrap();
        let saved = store.load().unwrap();
        assert!(saved.use_riwork_colors);
        assert!(!saved.remember_window_size);
        fs::write(dir.join("settings.json"), "broken").unwrap();
        assert!(
            store
                .update(|settings| settings.use_riwork_colors = false)
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(dir.join("settings.json")).unwrap(),
            "broken"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
