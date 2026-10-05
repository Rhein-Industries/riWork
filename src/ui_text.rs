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
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    settings::{Settings, SettingsStore},
    theme::{self, GhosttyConfigStamp, ThemeChoice},
};

/// Settings → Interface font, for the Native theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceFont {
    /// SF Pro for everything.
    System,
    /// SF Pro, with a monospace face for paths, branches, shortcut keys and counts.
    #[default]
    SystemMono,
}

impl InterfaceFont {
    pub const ALL: [Self; 2] = [Self::System, Self::SystemMono];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System (SF Pro)",
            Self::SystemMono => "System + monospace accents",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::System => Self::SystemMono,
            Self::SystemMono => Self::System,
        }
    }
}

/// The faces RiWork's interface is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// Menlo throughout: every theme but Native.
    Menlo,
    /// The system font throughout.
    System,
    /// The system font, with monospace accents.
    SystemMono,
}

impl Face {
    pub fn of(settings: &Settings) -> Self {
        if settings.theme != ThemeChoice::Native {
            return Self::Menlo;
        }
        match settings.interface_font {
            InterfaceFont::System => Self::System,
            InterfaceFont::SystemMono => Self::SystemMono,
        }
    }

    /// How many points larger than the saved size this face draws. Sizes were
    /// designed in Menlo, whose 11 px list text sits beside a 13 pt terminal. SF
    /// Pro is narrower and has a smaller x-height at equal size, and macOS's own
    /// lists, sidebars and menus use it at 13 pt, so the system face draws the
    /// default 11 pt list text at 13 pt, with tabs and captions growing alike.
    /// It is an offset, not a factor, so the size Settings shows (`shown_points`)
    /// is the size drawn and still steps by whole points. While the size matches
    /// the terminal it is Ghostty's number as it is, like Menlo's.
    pub fn offset(self, matching_terminal: bool) -> f32 {
        match self {
            Self::Menlo => 0.0,
            Self::System | Self::SystemMono if matching_terminal => 0.0,
            Self::System | Self::SystemMono => SYSTEM_BODY_POINTS - REFERENCE_SIZE,
        }
    }
}

/// macOS's body text size, which Native draws the 11 px design text at.
pub const SYSTEM_BODY_POINTS: f32 = 13.0;
const MENLO: &str = "Menlo";
const SYSTEM_FONT: &str = ".SystemUIFont";
const SF_MONO: &str = "SF Mono";

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
    static FACE: Cell<Face> = const { Cell::new(Face::Menlo) };
    /// Whether SF Mono is installed; Menlo stands in for it otherwise.
    static HAS_SF_MONO: Cell<bool> = const { Cell::new(true) };
}

/// The face the interface is drawn in now.
pub fn face() -> Face {
    FACE.with(Cell::get)
}

/// Whether the Native theme is what the interface is drawn in: it is the one
/// theme with the system face. Render code that has no `cx` asks this.
pub fn is_native() -> bool {
    face() != Face::Menlo
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

/// Names that keep their capital in sentence case.
const PROPER_NOUNS: [&str; 12] = [
    "Codex", "Claude", "Grok", "Orca", "Cua", "Mac", "Ghostty", "RiWork", "Vim", "Finder",
    "GitHub", "Git",
];

/// Abbreviations that stay in capitals.
const ACRONYMS: [&str; 14] = [
    "CPU", "RAM", "MCP", "ID", "URL", "PR", "SSH", "API", "OK", "UI", "AI", "CLI", "TCC", "PID",
];

/// Rewrites the words written in capitals in `text` into sentence case: a word of two or
/// more capital letters becomes lowercase, or capitalized where a sentence starts (the
/// text's start, or after "·", ":" or "."). Names (Codex, Claude, Orca…) keep their capital
/// and abbreviations (CPU, RAM…) stay capitals. Anything else, including mixed-case words,
/// ids such as A1 and paths, is left as it is.
pub fn sentence_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut sentence_start = true;
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String, sentence_start: &mut bool| {
        if word.is_empty() {
            return;
        }
        let caps = word.chars().count() >= 2 && word.chars().all(|c| c.is_ascii_uppercase());
        if caps && !ACRONYMS.contains(&word.as_str()) {
            let lower = word.to_ascii_lowercase();
            let proper = PROPER_NOUNS
                .iter()
                .find(|name| name.eq_ignore_ascii_case(&lower));
            match proper {
                Some(name) => out.push_str(name),
                None if *sentence_start => {
                    let mut chars = lower.chars();
                    if let Some(first) = chars.next() {
                        out.push(first.to_ascii_uppercase());
                        out.push_str(chars.as_str());
                    }
                }
                None => out.push_str(&lower),
            }
        } else {
            out.push_str(word);
        }
        *sentence_start = false;
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() {
            word.push(c);
        } else {
            flush(&mut word, &mut out, &mut sentence_start);
            if matches!(c, '·' | ':' | '.' | '/') {
                sentence_start = true;
            }
            out.push(c);
        }
    }
    flush(&mut word, &mut out, &mut sentence_start);
    out
}

/// The size Settings shows for the saved `points`: what the face draws.
pub fn shown_points(settings: &Settings, points: f32) -> f32 {
    points + Face::of(settings).offset(settings.ui_text_matches_terminal)
}

/// The font family of a window's or popup's root.
pub fn ui_family() -> SharedString {
    match face() {
        Face::Menlo => MENLO.into(),
        Face::System | Face::SystemMono => SYSTEM_FONT.into(),
    }
}

/// The font family of technical text: a path, a branch, an id, a shortcut key or
/// a count. Menlo or SF Mono, or the system font when accents are off.
pub fn mono_family() -> SharedString {
    match face() {
        Face::Menlo => MENLO.into(),
        Face::System => SYSTEM_FONT.into(),
        Face::SystemMono if HAS_SF_MONO.with(Cell::get) => SF_MONO.into(),
        Face::SystemMono => MENLO.into(),
    }
}

/// The font family of code: a code block, a diff, a command and its output. Unlike
/// `mono_family` it stays fixed-width whatever the accents setting, as an editor keeps its
/// code in a fixed-width face: Menlo in the colorful themes, SF Mono (else Menlo) in Native.
pub fn code_family() -> SharedString {
    match face() {
        Face::System | Face::SystemMono if HAS_SF_MONO.with(Cell::get) => SF_MONO.into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_native_draws_in_the_system_face_at_its_native_size() {
        let mut settings = Settings::default();
        for theme in ThemeChoice::ALL {
            settings.theme = theme;
            let face = Face::of(&settings);
            if theme == ThemeChoice::Native {
                assert_eq!(face, Face::SystemMono);
                // The default 11 pt is drawn, and shown, at macOS's 13 pt body size.
                assert_eq!(DEFAULT_POINTS + face.offset(false), SYSTEM_BODY_POINTS);
                assert_eq!(shown_points(&settings, 12.0), 14.0);
                // Matching the terminal shows Ghostty's own number.
                assert_eq!(face.offset(true), 0.0);
            } else {
                assert_eq!(face, Face::Menlo, "{theme:?}");
                assert_eq!(face.offset(false), 0.0);
                assert_eq!(shown_points(&settings, 12.0), 12.0);
            }
        }
        settings.theme = ThemeChoice::Native;
        settings.interface_font = InterfaceFont::System;
        assert_eq!(Face::of(&settings), Face::System);
        assert_eq!(InterfaceFont::System.other(), InterfaceFont::SystemMono);
        // Off the UI thread, and before `init`, the face is Menlo as it always was.
        assert_eq!(ui_family(), "Menlo");
        assert_eq!(mono_family(), "Menlo");
    }

    #[test]
    fn labels_are_capitals_in_the_colorful_themes_and_as_written_in_native() {
        assert!(!is_native());
        assert_eq!(cased("Add host"), "ADD HOST");
        assert_eq!(cased("+ Folder"), "+ FOLDER");
        for face in [Face::System, Face::SystemMono] {
            FACE.with(|cell| cell.set(face));
            assert!(is_native());
            assert_eq!(cased("Add host"), "Add host");
        }
        FACE.with(|cell| cell.set(Face::Menlo));
    }

    #[test]
    fn capitals_composed_at_run_time_read_in_sentence_case() {
        for (caps, sentence) in [
            ("CODEX · 7d 81% left", "Codex · 7d 81% left"),
            (
                "DEFAULT (APP) · System default",
                "Default (app) · System default",
            ),
            ("1 LIVE", "1 live"),
            ("USAGE · LOADING", "Usage · Loading"),
            ("CPU 0.0% RAM 5.5 MiB", "CPU 0.0% RAM 5.5 MiB"),
            ("CODEX A1 · me@example.com", "Codex A1 · me@example.com"),
            ("NO PROJECTS", "No projects"),
            ("RIWORK / PREFERENCES", "RiWork / Preferences"),
            ("zsh 06 · main", "zsh 06 · main"),
        ] {
            assert_eq!(sentence_case(caps), sentence, "{caps}");
        }
        // Off the UI thread the theme is a colorful one, which keeps the capitals.
        assert_eq!(quiet("1 LIVE"), "1 LIVE");
    }

    #[test]
    fn faces_name_their_families() {
        let families = |face| {
            FACE.with(|cell| cell.set(face));
            let names = (ui_family(), mono_family());
            FACE.with(|cell| cell.set(Face::Menlo));
            names
        };
        assert_eq!(families(Face::Menlo), ("Menlo".into(), "Menlo".into()));
        assert_eq!(
            families(Face::System),
            (".SystemUIFont".into(), ".SystemUIFont".into())
        );
        assert_eq!(
            families(Face::SystemMono),
            (".SystemUIFont".into(), "SF Mono".into())
        );
        HAS_SF_MONO.with(|cell| cell.set(false));
        assert_eq!(families(Face::SystemMono).1, "Menlo");
        HAS_SF_MONO.with(|cell| cell.set(true));
    }

    #[test]
    fn code_stays_fixed_width_in_every_face() {
        let code = |face| {
            FACE.with(|cell| cell.set(face));
            let family = code_family();
            FACE.with(|cell| cell.set(Face::Menlo));
            family
        };
        assert_eq!(code(Face::Menlo), "Menlo");
        // Even with the accents off, which set technical text in the system font.
        assert_eq!(code(Face::System), "SF Mono");
        assert_eq!(code(Face::SystemMono), "SF Mono");
        HAS_SF_MONO.with(|cell| cell.set(false));
        assert_eq!(code(Face::System), "Menlo");
        HAS_SF_MONO.with(|cell| cell.set(true));
    }

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
            assert_eq!(
                is_size_keystroke(&Keystroke::parse(text).unwrap()),
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn sizes_are_unchanged_at_the_default_scale() {
        assert_eq!(scale(), 1.0);
        assert_eq!(text(9.0), px(9.0));
        assert_eq!(space(22.0), px(22.0));
    }
}
