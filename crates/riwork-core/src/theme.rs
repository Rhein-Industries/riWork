//! Serialized theme selection shared with the desktop.

use serde::{Deserialize, Serialize};

/// RiWork's one theme. Kept as a serialized choice so `settings.json` keeps its
/// `theme` key: a value an older build wrote (a theme since removed) reads as
/// Native, like any other value this build does not know.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    Native,
}

/// Pure packed RGB terminal colors. This type has no Ghostty or GUI dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalTheme {
    pub background: u32,
    pub foreground: u32,
    pub palette: [u32; 16],
}

fn terminal_theme(background: u32, foreground: u32, palette: [u32; 16]) -> TerminalTheme {
    TerminalTheme {
        background,
        foreground,
        palette,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub bg: u32,
    pub panel: u32,
    pub panel_active: u32,
    pub divider: u32,
    pub cyan: u32,
    pub magenta: u32,
    pub gold: u32,
    pub text: u32,
    pub muted: u32,
    /// The keyboard focus ring of a control: the primary color, since the one
    /// signal color is kept for state that must stand out. Not published.
    pub focus: u32,
    /// What says an agent is working: the signal color (`gold`). Not published.
    pub working: u32,
}

impl Palette {
    /// Native in light mode, on the published tokens: `cyan`, the primary and
    /// selection color, is black; `magenta`, the secondary accent, a dark grey;
    /// `gold` is the one signal color, a deep orange for working agents, errors
    /// and warnings. Every text color reads at 4.5:1 or better on `panel_active`.
    pub const NATIVE_LIGHT: Self = Self {
        bg: 0xffffff,
        panel: 0xf5f5f7,
        panel_active: 0xe8e8ed,
        divider: 0xd2d2d7,
        cyan: 0x000000,
        magenta: 0x3a3a3c,
        gold: 0xb34000,
        text: 0x1d1d1f,
        muted: 0x636366,
        focus: 0x000000,
        working: 0xb34000,
    };

    /// Native in dark mode: the same roles inverted, white on black, with the
    /// signal color lifted to macOS's dark-mode orange.
    pub const NATIVE_DARK: Self = Self {
        bg: 0x000000,
        panel: 0x1c1c1e,
        panel_active: 0x2c2c2e,
        divider: 0x3a3a3c,
        cyan: 0xffffff,
        magenta: 0xc7c7cc,
        gold: 0xff9f0a,
        text: 0xf5f5f7,
        muted: 0x98989d,
        focus: 0xffffff,
        working: 0xff9f0a,
    };

    pub const fn native(dark: bool) -> Self {
        if dark {
            Self::NATIVE_DARK
        } else {
            Self::NATIVE_LIGHT
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Appearance {
    pub selected: ThemeChoice,
    pub palette: Palette,
    /// Native's terminal colors for the resolved system appearance.
    pub terminal: TerminalTheme,
    /// The terminals' own Ghostty colors, where a caller knows them.
    pub ghostty: Option<TerminalTheme>,
}

impl Appearance {
    pub fn resolve(choice: ThemeChoice, system_dark: bool) -> Self {
        Self {
            selected: choice,
            palette: Palette::native(system_dark),
            terminal: native_terminal_theme(system_dark),
            ghostty: None,
        }
    }

    /// What terminals are forced to: Native's colors while "Terminals match
    /// the theme" is on, nothing (Ghostty's own colors) while it is off.
    pub fn terminal_override(&self, force: bool) -> Option<TerminalTheme> {
        force.then_some(self.terminal)
    }

    pub fn shown_terminal(&self, force: bool) -> Option<TerminalTheme> {
        self.terminal_override(force).or(self.ghostty)
    }

    pub fn is_stale_for(&self, system_dark: bool) -> bool {
        self.palette != Palette::native(system_dark)
    }

    pub fn diff_colors(&self, force: bool) -> DiffColors {
        DiffColors::from_terminal(
            &self.shown_terminal(force).unwrap_or(self.terminal),
            self.palette.panel_active,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffColors {
    pub added: u32,
    pub removed: u32,
}
impl DiffColors {
    pub fn from_terminal(theme: &TerminalTheme, background: u32) -> Self {
        Self {
            added: readable(theme.palette[2], background),
            removed: readable(theme.palette[1], background),
        }
    }
}

/// Native's terminal colors: the skin's own white or black background and a
/// restrained ANSI palette. Each of the eight normal colors but the one that is
/// the background's own (black on black) reads at 4.5:1 or better on it; the
/// bright ones are a step lighter (dark) or brighter (light) of the same hues.
pub fn native_terminal_theme(dark: bool) -> TerminalTheme {
    if dark {
        terminal_theme(
            0x000000,
            0xf5f5f7,
            [
                0x2c2c2e, 0xff6b60, 0x5fd07a, 0xe6c35c, 0x64a8ff, 0xc58af9, 0x5ac8d8, 0xc7c7cc,
                0x636366, 0xff8a80, 0x85e09c, 0xf2d68a, 0x8cbfff, 0xd9a8ff, 0x8ad9e5, 0xffffff,
            ],
        )
    } else {
        terminal_theme(
            0xffffff,
            0x1d1d1f,
            [
                0x1d1d1f, 0xc4281c, 0x1f7a37, 0x8a5a00, 0x1d5fbf, 0x8b3dbf, 0x0f6f80, 0x636366,
                0x8e8e93, 0xd93a2b, 0x2a8f46, 0xa66d00, 0x2f74d9, 0xa24fd6, 0x168596, 0x3a3a3c,
            ],
        )
    }
}

/// `first` moved `amount` (0 to 1) of the way to `second`, channel by channel.
pub fn mix(first: u32, second: u32, amount: f64) -> u32 {
    let mut result = 0;
    for shift in [16, 8, 0] {
        let first = f64::from((first >> shift) & 255);
        let second = f64::from((second >> shift) & 255);
        result |= ((first + (second - first) * amount).round() as u32) << shift;
    }
    result
}

pub fn luminance(color: u32) -> f64 {
    let channel = |shift| {
        let value = f64::from((color >> shift) & 255u32) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// A background reads as dark when its relative luminance is below one half.
pub fn is_dark(color: u32) -> bool {
    luminance(color) < 0.5
}

/// The WCAG contrast ratio of two colors, from 1 to 21.
pub fn contrast(first: u32, second: u32) -> f64 {
    let first = luminance(first);
    let second = luminance(second);
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

fn readable(foreground: u32, background: u32) -> u32 {
    if contrast(foreground, background) >= 4.5 {
        return foreground;
    }
    let target = if contrast(0xffffff, background) > contrast(0, background) {
        0xffffff
    } else {
        0
    };
    for step in 1..=100 {
        let candidate = mix(foreground, target, f64::from(step) / 100.0);
        if contrast(candidate, background) >= 4.5 {
            return candidate;
        }
    }
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn every_removed_theme_name_reads_as_native_and_round_trips_untouched() {
        for name in [
            "native",
            "ghostty",
            "ri_work",
            "catppuccin",
            "tokyo_night",
            "gruvbox_light",
            "hermes",
            "future_theme",
        ] {
            let settings: Settings = serde_json::from_value(serde_json::json!({
                "theme": name, "chat_display_modes": {"saved-chat": "verbose"}
            }))
            .unwrap();
            assert_eq!(settings.theme, ThemeChoice::Native);
            let saved = serde_json::to_value(&settings).unwrap();
            assert_eq!(saved["theme"], "native");
            assert_eq!(saved["chat_display_modes"]["saved-chat"], "verbose");
        }
        assert_eq!(ThemeChoice::default(), ThemeChoice::Native);
    }

    #[test]
    fn native_terminals_match_the_theme_only_while_the_option_is_on() {
        for dark in [false, true] {
            let appearance = Appearance::resolve(ThemeChoice::Native, dark);
            assert_eq!(
                appearance.terminal_override(true),
                Some(native_terminal_theme(dark))
            );
            assert_eq!(appearance.terminal_override(false), None);
            assert_eq!(appearance.palette, Palette::native(dark));
        }
        let mut settings = Settings::default();
        assert!(settings.native_terminal_colors);
        assert!(settings.terminal_colors_forced());
        settings.native_terminal_colors = false;
        assert_eq!(
            Appearance::resolve(settings.theme, false)
                .terminal_override(settings.terminal_colors_forced()),
            None
        );
    }

    #[test]
    fn native_goes_stale_when_macos_switches_light_and_dark() {
        let light = Appearance::resolve(ThemeChoice::Native, false);
        let dark = Appearance::resolve(ThemeChoice::Native, true);
        assert!(!light.is_stale_for(false));
        assert!(!dark.is_stale_for(true));
        assert!(light.is_stale_for(true));
        assert!(dark.is_stale_for(false));
    }

    /// The P1 successor inspects effective colors without an appearance file writer.
    #[test]
    fn shown_terminal_is_ghosttys_own_while_terminals_do_not_match_the_theme() {
        let mut appearance = Appearance::resolve(ThemeChoice::Native, false);
        assert_eq!(appearance.shown_terminal(false), None);
        let ghostty = terminal_theme(0x282c34, 0xabb2bf, [0x101010; 16]);
        appearance.ghostty = Some(ghostty);
        assert_eq!(appearance.shown_terminal(false), Some(ghostty));
        assert_eq!(
            appearance.shown_terminal(true),
            Some(native_terminal_theme(false))
        );
    }

    #[test]
    fn diff_colors_use_the_shown_ansi_colors_at_wcag_contrast() {
        for dark in [false, true] {
            for force in [false, true] {
                let appearance = Appearance::resolve(ThemeChoice::Native, dark);
                let terminal = appearance.shown_terminal(force).unwrap_or(appearance.terminal);
                let diff = appearance.diff_colors(force);
                assert_eq!(
                    diff.added,
                    readable(terminal.palette[2], appearance.palette.panel_active)
                );
                assert_eq!(
                    diff.removed,
                    readable(terminal.palette[1], appearance.palette.panel_active)
                );
                assert!(contrast(diff.added, appearance.palette.panel_active) >= 4.5);
                assert!(contrast(diff.removed, appearance.palette.panel_active) >= 4.5);
            }
        }
        assert!(is_dark(0));
        assert!(!is_dark(0xffffff));
        assert!(is_dark(0xbbbbbb));
        assert!(!is_dark(0xbcbcbc));
    }
}
