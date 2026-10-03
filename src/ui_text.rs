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

use std::cell::Cell;

use gpui::{App, Global, Keystroke, Pixels, px};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    settings::{Settings, SettingsStore},
    theme::{self, GhosttyConfigStamp},
};

/// The size the text-size setting names: the 11 px list text of the panels
/// (Projects, Files, Preview, Shells), the UI text read most. At the default
/// 11 pt every size is as designed; tab, status bar and caption text (9–10 px)
/// stays a little smaller than the named size, as it is today. MATCH TERMINAL
/// sets this text to Ghostty's `font-size`.
///
/// Calibrated by measurement, not by nominal size alone. Ghostty's default font
/// on macOS is its embedded JetBrains Mono (no `font-family` set; see
/// vendor/ghostty/src/font/embedded.zig); RiWork's text is Menlo. Screenshots of
/// the running app on a 1x display, terminal at `font-size = 20` beside 20 px UI
/// text: lowercase x-height 11.1 px (terminal) against 11.2 px (Menlo), advance
/// 12.2 against 12.0 px. The two fonts match within 2 % at equal size, and GPUI
/// pixels are Ghostty points. Matching the 10 px workspace text instead put the
/// 11 px panel text at 14.3 px beside a 13 pt terminal (an 8.6 px advance against
/// the terminal's 8 px cell), visibly larger; matching this text puts it at 13 px.
pub const REFERENCE_SIZE: f32 = 11.0;
pub const DEFAULT_POINTS: f32 = REFERENCE_SIZE;
pub const MIN_POINTS: f32 = 9.0;
pub const MAX_POINTS: f32 = 24.0;
/// ⌘+ / ⌘− and the row's − / +, like Ghostty's default `increase_font_size:1`.
pub const STEP_POINTS: f32 = 1.0;
/// Ghostty's built-in `font-size` on macOS (13; 12 elsewhere), from `@"font-size"`
/// in vendor/ghostty/src/config/Config.zig. Used only if the config cannot be read.
pub const GHOSTTY_DEFAULT_FONT_SIZE: f32 = 13.0;

thread_local! {
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
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

/// A saved text size in points, kept to a tenth of a point so `Settings` stays
/// `Eq`. It is written to `settings.json` as a plain number (`11`, `12.5`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPoints(u16);

impl TextPoints {
    pub const DEFAULT: Self = Self(110);

    /// Rounded to a tenth and pulled into `MIN_POINTS..=MAX_POINTS`; a size that
    /// is not a number is the default.
    pub fn new(points: f32) -> Self {
        if !points.is_finite() {
            return Self::DEFAULT;
        }
        Self((points.clamp(MIN_POINTS, MAX_POINTS) * 10.0).round() as u16)
    }

    pub fn points(self) -> f32 {
        f32::from(self.0) / 10.0
    }

    pub fn clamped(self) -> Self {
        Self::new(self.points())
    }
}

impl Default for TextPoints {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Serialize for TextPoints {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0.is_multiple_of(10) {
            serializer.serialize_u16(self.0 / 10)
        } else {
            serializer.serialize_f64(f64::from(self.0) / 10.0)
        }
    }
}

impl<'de> Deserialize<'de> for TextPoints {
    /// Any number, pulled into range; anything else fails, which `Settings`
    /// reads as the default.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let points = f64::deserialize(deserializer)?;
        Ok(Self::new(points as f32))
    }
}

/// "11 pt", or "12.5 pt" for a fractional size.
pub fn label(points: f32) -> String {
    let tenths = (points * 10.0).round();
    if tenths % 10.0 == 0.0 {
        format!("{} pt", tenths / 10.0)
    } else {
        format!("{:.1} pt", tenths / 10.0)
    }
}

/// The size MATCH TERMINAL shows for Ghostty's `font_size`: the same number,
/// within range.
pub fn matching_points(font_size: f32) -> f32 {
    let font_size = if font_size.is_finite() && font_size > 0.0 {
        font_size
    } else {
        GHOSTTY_DEFAULT_FONT_SIZE
    };
    TextPoints::new(font_size).points()
}

/// One point bigger or smaller, onto whole points: 11 → 12, 12.5 → 13 or 12.
pub fn stepped(points: f32, bigger: bool) -> f32 {
    let points = TextPoints::new(points).points();
    let next = if bigger {
        (points / STEP_POINTS).floor() * STEP_POINTS + STEP_POINTS
    } else {
        (points / STEP_POINTS).ceil() * STEP_POINTS - STEP_POINTS
    };
    TextPoints::new(next).points()
}

/// The size shown, given the terminal font size when it is known.
pub fn effective_points(settings: &Settings, terminal_font_size: Option<f32>) -> f32 {
    if settings.ui_text_matches_terminal {
        matching_points(terminal_font_size.unwrap_or(GHOSTTY_DEFAULT_FONT_SIZE))
    } else {
        settings.ui_text_size.clamped().points()
    }
}

pub fn current_points(cx: &App) -> f32 {
    effective_points(cx.global::<Settings>(), terminal_font_size(cx))
}

/// Ghostty's effective `font-size`, read while matching it.
pub fn terminal_font_size(cx: &App) -> Option<f32> {
    cx.try_global::<TerminalFontSize>().map(|font| font.size)
}

/// What the text-size commands do to the saved settings. Stepping while
/// matching the terminal stops matching and steps from the size it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeChange {
    Bigger,
    Smaller,
    Reset,
}

impl SizeChange {
    /// Steps from the size `settings` itself shows, so a command applied to the
    /// settings just read under the store's lock starts from what another window
    /// or process saved last, not from this process's copy. `terminal_font_size`
    /// is Ghostty's `font-size`, for settings that match it.
    pub fn apply(self, settings: &mut Settings, terminal_font_size: f32) {
        let shown = effective_points(settings, Some(terminal_font_size));
        settings.ui_text_matches_terminal = false;
        settings.ui_text_size = TextPoints::new(match self {
            Self::Bigger => stepped(shown, true),
            Self::Smaller => stepped(shown, false),
            Self::Reset => DEFAULT_POINTS,
        });
    }
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
    let scale = current_points(cx) / REFERENCE_SIZE;
    if SCALE.with(|cell| cell.replace(scale)) != scale {
        crate::tooltip::hide(cx);
        cx.refresh_windows();
    }
}

/// Ghostty's `font-size` now: the cached value while its config files are
/// unchanged, else read again. A size command needs it even when this process
/// was not matching, since another process may have turned matching on.
fn ghostty_font_size_now(cx: &mut App) -> f32 {
    let stamp = theme::ghostty_config_stamp();
    if let Some(font) = cx.try_global::<TerminalFontSize>().filter(|font| font.stamp == stamp) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_whole_points_and_stop_at_the_limits() {
        assert_eq!(stepped(11.0, true), 12.0);
        assert_eq!(stepped(11.0, false), 10.0);
        assert_eq!(stepped(12.5, true), 13.0);
        assert_eq!(stepped(12.5, false), 12.0);
        assert_eq!(stepped(MAX_POINTS, true), MAX_POINTS);
        assert_eq!(stepped(MIN_POINTS, false), MIN_POINTS);
        assert_eq!(stepped(40.0, false), 23.0);
        assert_eq!(stepped(2.0, true), 10.0);
    }

    #[test]
    fn matching_shows_ghosttys_own_number() {
        assert_eq!(matching_points(GHOSTTY_DEFAULT_FONT_SIZE), 13.0);
        assert_eq!(matching_points(13.5), 13.5);
        assert_eq!(matching_points(4.0), MIN_POINTS);
        assert_eq!(matching_points(72.0), MAX_POINTS);
        assert_eq!(matching_points(f32::NAN), 13.0);
        assert_eq!(matching_points(-1.0), 13.0);
        assert_eq!(label(13.0), "13 pt");
        assert_eq!(label(13.5), "13.5 pt");
    }

    #[test]
    fn the_shown_size_follows_the_terminal_only_while_matching() {
        let mut settings = Settings {
            ui_text_size: TextPoints::new(15.0),
            ..Settings::default()
        };
        assert_eq!(effective_points(&settings, Some(16.5)), 15.0);
        settings.ui_text_matches_terminal = true;
        assert_eq!(effective_points(&settings, Some(16.5)), 16.5);
        assert_eq!(effective_points(&settings, None), 13.0);
    }

    #[test]
    fn saved_sizes_round_to_tenths_clamp_and_write_plain_numbers() {
        assert_eq!(TextPoints::default().points(), DEFAULT_POINTS);
        assert_eq!(TextPoints::new(12.34).points(), 12.3);
        assert_eq!(TextPoints::new(100.0).points(), MAX_POINTS);
        assert_eq!(TextPoints::new(f32::INFINITY), TextPoints::DEFAULT);
        assert_eq!(serde_json::to_string(&TextPoints::new(12.0)).unwrap(), "12");
        assert_eq!(serde_json::to_string(&TextPoints::new(12.5)).unwrap(), "12.5");
        assert_eq!(serde_json::from_str::<TextPoints>("3").unwrap().points(), MIN_POINTS);
        assert!(serde_json::from_str::<TextPoints>(r#""big""#).is_err());
    }

    #[test]
    fn commands_stop_matching_and_step_from_the_shown_size() {
        let matching = Settings {
            ui_text_matches_terminal: true,
            ..Settings::default()
        };
        for (change, expected) in [
            (SizeChange::Bigger, 14.0),
            (SizeChange::Smaller, 12.0),
            (SizeChange::Reset, DEFAULT_POINTS),
        ] {
            let mut settings = matching.clone();
            change.apply(&mut settings, 13.0);
            assert!(!settings.ui_text_matches_terminal);
            assert_eq!(settings.ui_text_size.points(), expected);
        }
    }

    #[test]
    fn a_step_starts_from_the_stored_size_not_the_cached_one() {
        let dir = std::env::temp_dir().join(format!("riwork-text-step-{}", uuid::Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        // This process still holds 11 pt while another one saved 15 pt.
        let cached = Settings::default();
        store.update(|settings| settings.ui_text_size = TextPoints::new(15.0)).unwrap();
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
        store.update(|settings| settings.ui_text_matches_terminal = true).unwrap();
        let saved = store
            .update(|settings| SizeChange::Smaller.apply(settings, 20.0))
            .unwrap();
        assert!(!saved.ui_text_matches_terminal);
        assert_eq!(saved.ui_text_size.points(), 19.0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_the_menu_keystrokes_count_as_text_size_keys() {
        for (text, expected) in [
            ("cmd-=", true),
            ("cmd-shift-=", true),
            ("cmd-+", true),
            ("cmd--", true),
            ("cmd-0", true),
            ("cmd-shift-0", false),
            ("cmd-alt-=", false),
            ("ctrl-=", false),
            ("=", false),
            ("cmd-1", false),
        ] {
            assert_eq!(is_size_keystroke(&Keystroke::parse(text).unwrap()), expected, "{text}");
        }
    }

    #[test]
    fn sizes_are_unchanged_at_the_default_scale() {
        assert_eq!(scale(), 1.0);
        assert_eq!(text(9.0), px(9.0));
        assert_eq!(space(22.0), px(22.0));
    }
}
