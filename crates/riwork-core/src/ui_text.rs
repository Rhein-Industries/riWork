//! Persisted interface typography and pure text-size helpers.

use crate::settings::Settings;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Settings → Interface font, independent of the selected color palette.
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
    /// Menlo throughout for the colorful themes and Follow Ghostty.
    Menlo,
    /// The system font throughout.
    System,
    /// The system font, with monospace accents.
    SystemMono,
    /// Proportional Hermes interface text with fixed-width technical accents.
    Hermes,
}

impl Face {
    pub fn of(settings: &Settings) -> Self {
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
            Self::System | Self::SystemMono | Self::Hermes if matching_terminal => 0.0,
            Self::System | Self::SystemMono | Self::Hermes => SYSTEM_BODY_POINTS - REFERENCE_SIZE,
        }
    }
}

/// macOS's body text size, which Native draws the 11 px design text at.
pub const SYSTEM_BODY_POINTS: f32 = 13.0;

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

/// Names that keep their capital in sentence case.
const PROPER_NOUNS: [&str; 13] = [
    "Codex", "Claude", "Grok", "Muse", "Orca", "Cua", "Mac", "Ghostty", "RiWork", "Vim", "Finder",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SettingsStore;

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
