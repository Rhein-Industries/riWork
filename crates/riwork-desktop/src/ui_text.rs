//! The size of RiWork's own interface text: panels, tabs, menus, the status bar,
//! Settings and hover hints. Terminals keep Ghostty's font size and zoom.
//!
//! Like Ghostty's `font-size`, the size is in points: the size of the panels'
//! list text, 11 pt today, saved in `Settings` or taken from Ghostty's
//! `font-size` while MATCH TERMINAL (Settings → Text size) is on. ⌘+ / ⌘−
//! step it by one point, as Ghostty's own `increase_font_size:1` does. Render
//! code asks for `text(base)` and `space(base)` with the size it was designed at
//! (11 pt); everything scales by `size / REFERENCE_SIZE`. The effective scale
//! lives in a UI-thread cell rather than being read from the `Settings` global
//! at every call, so free helpers without a `cx` scale too; it is kept in step
//! with the globals by `init`, which also redraws every window when it changes.
//! Other threads, tests included, always see the design sizes.
//!
//! The face is decided here too. The colorful themes draw everything in Menlo,
//! as RiWork always has. Native draws in the system font (SF Pro) and, with
//! Settings → Interface font on "System + monospace accents", keeps a monospace
//! face (SF Mono, else Menlo) for technical text: paths, branches, ids, shortcut
//! keys and counts. Render code asks `ui_family()` for a root and
//! `mono_family()` for such text instead of naming a font.

use std::cell::Cell;

use gpui::{App, Global, Keystroke, Pixels, SharedString, px};
pub use riwork_core::ui_text::{
    DEFAULT_POINTS, Face, GHOSTTY_DEFAULT_FONT_SIZE, InterfaceFont, MAX_POINTS, MIN_POINTS,
    REFERENCE_SIZE, STEP_POINTS, SYSTEM_BODY_POINTS, SizeChange, TextPoints, effective_points,
    label, matching_points, sentence_case, shown_points, stepped,
};

use crate::{
    settings::{Settings, SettingsStore},
    theme::{self, GhosttyConfigStamp, ThemeChoice},
};

const MENLO: &str = "Menlo";
const SYSTEM_FONT: &str = ".SystemUIFont";
const SF_MONO: &str = "SF Mono";

thread_local! {
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
    static FACE: Cell<Face> = const { Cell::new(Face::Menlo) };
    /// Whether SF Mono is installed; Menlo stands in for it otherwise.
    static HAS_SF_MONO: Cell<bool> = const { Cell::new(true) };
}

/// The face the interface is drawn in now.
pub fn face() -> Face {
    FACE.with(Cell::get)
}

/// Sets the interface scale and face directly, for tests that lay out views; returns what
/// they were, to put back.
#[cfg(test)]
pub fn set_for_tests(scale: f32, face: Face) -> (f32, Face) {
    (
        SCALE.with(|cell| cell.replace(scale)),
        FACE.with(|cell| cell.replace(face)),
    )
}

/// Whether the Native theme is what the interface is drawn in: it is the one
/// theme with the system face. Render code that has no `cx` asks this.
pub fn is_native() -> bool {
    matches!(face(), Face::System | Face::SystemMono)
}

/// A label as Native shows it: in sentence case, as it is written in the source.
/// The colorful themes show every label in capitals, as RiWork always has.
pub fn cased(text: impl Into<SharedString>) -> SharedString {
    let text = text.into();
    if is_native() {
        text
    } else {
        text.to_uppercase().into()
    }
}

/// Text composed at run time with capitalized words in it, such as "CODEX · 7d 81% left"
/// or "1 LIVE", as the theme shows it: as it is in the colorful themes, in sentence case
/// in Native (see `sentence_case`).
pub fn quiet(text: impl Into<SharedString>) -> SharedString {
    let text = text.into();
    if is_native() {
        sentence_case(&text).into()
    } else {
        text
    }
}

/// The font family of a window's or popup's root.
pub fn ui_family() -> SharedString {
    match face() {
        Face::Menlo => MENLO.into(),
        Face::System | Face::SystemMono | Face::Hermes => SYSTEM_FONT.into(),
    }
}

/// The font family of technical text: a path, a branch, an id, a shortcut key or
/// a count. Menlo or SF Mono, or the system font when accents are off.
pub fn mono_family() -> SharedString {
    match face() {
        Face::Menlo => MENLO.into(),
        Face::System => SYSTEM_FONT.into(),
        Face::SystemMono | Face::Hermes if HAS_SF_MONO.with(Cell::get) => SF_MONO.into(),
        Face::SystemMono | Face::Hermes => MENLO.into(),
    }
}

/// The font family of code: a code block, a diff, a command and its output. Unlike
/// `mono_family` it stays fixed-width whatever the accents setting, as an editor keeps its
/// code in a fixed-width face: Menlo in the colorful themes, SF Mono (else Menlo) in Native.
pub fn code_family() -> SharedString {
    match face() {
        Face::System | Face::SystemMono | Face::Hermes if HAS_SF_MONO.with(Cell::get) => {
            SF_MONO.into()
        }
        _ => MENLO.into(),
    }
}

/// The effective scale: 1.0 is today's exact sizes.
pub fn scale() -> f32 {
    SCALE.with(Cell::get)
}

/// A text size designed at `base` px.
pub fn text(base: f32) -> Pixels {
    px(base * scale())
}

/// A layout length that holds text (a bar's height, a fixed-width cell): it grows
/// with the text and stays whole pixels, but never shrinks below its design size,
/// so smaller text leaves room around native chrome such as the window controls.
pub fn space_f32(base: f32) -> f32 {
    (base * scale().max(1.0)).round()
}

pub fn space(base: f32) -> Pixels {
    px(space_f32(base))
}

pub fn current_points(cx: &App) -> f32 {
    effective_points(cx.global::<Settings>(), terminal_font_size(cx))
}

/// Ghostty's effective `font-size`, read while matching it.
pub fn terminal_font_size(cx: &App) -> Option<f32> {
    cx.try_global::<TerminalFontSize>().map(|font| font.size)
}

/// The keystrokes of the View menu's text-size items: ⌘= / ⌘+, ⌘- and ⌘0.
pub fn is_size_keystroke(keystroke: &Keystroke) -> bool {
    let modifiers = keystroke.modifiers;
    if !modifiers.platform || modifiers.control || modifiers.alt || modifiers.function {
        return false;
    }
    match keystroke.key.as_str() {
        "=" | "+" => true,
        "-" | "0" => !modifiers.shift,
        _ => false,
    }
}

/// Ghostty's `font-size` and the config files it was read from.
pub struct TerminalFontSize {
    stamp: GhosttyConfigStamp,
    size: f32,
}

impl Global for TerminalFontSize {}

/// Reads Ghostty's `font-size` again when matching it and a config file changed.
/// Called on every settings change and on the two-second appearance poll.
pub fn refresh_terminal_font_size(cx: &mut App) {
    if !cx.global::<Settings>().ui_text_matches_terminal {
        return;
    }
    // Taken before parsing, so an edit made during the parse shows up next poll.
    let stamp = theme::ghostty_config_stamp();
    if cx
        .try_global::<TerminalFontSize>()
        .is_some_and(|font| font.stamp == stamp)
    {
        return;
    }
    let size = theme::read_ghostty_font_size().unwrap_or_else(|error| {
        eprintln!("riwork: {error}");
        GHOSTTY_DEFAULT_FONT_SIZE
    });
    cx.set_global(TerminalFontSize { stamp, size });
}

/// Brings the scale in line with the globals and redraws every window if it moved.
/// A hint on screen or about to open was measured at the old scale; its popup
/// window would keep that size, so it goes until the pointer rests again.
fn sync(cx: &mut App) {
    let settings = cx.global::<Settings>();
    let face = Face::of(settings);
    let scale =
        (current_points(cx) + face.offset(settings.ui_text_matches_terminal)) / REFERENCE_SIZE;
    let scale_moved = SCALE.with(|cell| cell.replace(scale)) != scale;
    let face_moved = FACE.with(|cell| cell.replace(face)) != face;
    if scale_moved || face_moved {
        crate::tooltip::hide(cx);
        cx.refresh_windows();
    }
}

/// Ghostty's `font-size` now: the cached value while its config files are
/// unchanged, else read again. A size command needs it even when this process
/// was not matching, since another process may have turned matching on.
fn ghostty_font_size_now(cx: &mut App) -> f32 {
    let stamp = theme::ghostty_config_stamp();
    if let Some(font) = cx
        .try_global::<TerminalFontSize>()
        .filter(|font| font.stamp == stamp)
    {
        return font.size;
    }
    let size = theme::read_ghostty_font_size().unwrap_or_else(|error| {
        eprintln!("riwork: {error}");
        GHOSTTY_DEFAULT_FONT_SIZE
    });
    cx.set_global(TerminalFontSize { stamp, size });
    size
}

/// Saves a text-size command to `store` and returns the saved settings, for the
/// shortcuts, the View menu and the Settings row alike. The step starts from the
/// settings read under the store's lock.
pub fn save(store: &SettingsStore, change: SizeChange, cx: &mut App) -> Result<Settings, String> {
    let terminal_font_size = ghostty_font_size_now(cx);
    store.update(|settings| change.apply(settings, terminal_font_size))
}

/// Call once both `Settings` and the window-independent globals exist.
pub fn init(cx: &mut App) {
    let has_sf_mono = cx
        .text_system()
        .all_font_names()
        .iter()
        .any(|name| name == SF_MONO);
    HAS_SF_MONO.with(|cell| cell.set(has_sf_mono));
    refresh_terminal_font_size(cx);
    sync(cx);
    cx.observe_global::<Settings>(|cx| {
        refresh_terminal_font_size(cx);
        sync(cx);
    })
    .detach();
    cx.observe_global::<TerminalFontSize>(sync).detach();
}

/// Saves a text-size command, for the menu and the shortcuts.
pub fn change(change: SizeChange, cx: &mut App) {
    match SettingsStore::open_default().and_then(|store| save(&store, change, cx)) {
        Ok(settings) => cx.set_global(settings),
        Err(error) => eprintln!("riwork: {error}"),
    }
}

/// How wide `text` is drawn on one line in `family` at `size`, in px.
pub fn line_width(
    text: &str,
    family: SharedString,
    size: Pixels,
    bold: bool,
    window: &gpui::Window,
) -> f32 {
    let mut font = gpui::font(family);
    if bold {
        font.weight = gpui::FontWeight::BOLD;
    }
    let run = gpui::TextRun {
        len: text.len(),
        font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line =
        window
            .text_system()
            .shape_line(SharedString::from(text.to_owned()), size, &[run], None);
    f32::from(line.width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_sizes_round_to_tenths_clamp_and_write_plain_numbers() {
        assert_eq!(TextPoints::default().points(), DEFAULT_POINTS);
        assert_eq!(TextPoints::new(12.34).points(), 12.3);
        assert_eq!(TextPoints::new(100.0).points(), MAX_POINTS);
        assert_eq!(TextPoints::new(f32::INFINITY), TextPoints::DEFAULT);
        assert_eq!(serde_json::to_string(&TextPoints::new(12.0)).unwrap(), "12");
        assert_eq!(
            serde_json::to_string(&TextPoints::new(12.5)).unwrap(),
            "12.5"
        );
        assert_eq!(
            serde_json::from_str::<TextPoints>("3").unwrap().points(),
            MIN_POINTS
        );
        assert!(serde_json::from_str::<TextPoints>(r#""big""#).is_err());
    }

    #[test]
    fn a_step_starts_from_the_stored_size_not_the_cached_one() {
        let dir = std::env::temp_dir().join(format!("riwork-text-step-{}", uuid::Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        // This process still holds 11 pt while another one saved 15 pt.
        let cached = Settings::default();
        store
            .update(|settings| settings.ui_text_size = TextPoints::new(15.0))
            .unwrap();
        assert_eq!(effective_points(&cached, None), 11.0);
        let saved = store
            .update(|settings| SizeChange::Bigger.apply(settings, 13.0))
            .unwrap();
        assert_eq!(saved.ui_text_size.points(), 16.0);
        // Two quick steps each start from the one before.
        let saved = store
            .update(|settings| SizeChange::Bigger.apply(settings, 13.0))
            .unwrap();
        assert_eq!(saved.ui_text_size.points(), 17.0);
        // Another process turned matching on: the step starts from Ghostty's size.
        store
            .update(|settings| settings.ui_text_matches_terminal = true)
            .unwrap();
        let saved = store
            .update(|settings| SizeChange::Smaller.apply(settings, 20.0))
            .unwrap();
        assert!(!saved.ui_text_matches_terminal);
        assert_eq!(saved.ui_text_size.points(), 19.0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
