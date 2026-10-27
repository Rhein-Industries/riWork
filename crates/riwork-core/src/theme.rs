//! Serialized theme selection shared with the desktop.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    Ghostty,
    Native,
    RiWork,
    Catppuccin,
    TokyoNight,
    GruvboxLight,
    Hermes,
}

impl ThemeChoice {
    pub const ALL: [Self; 7] = [
        Self::Ghostty,
        Self::Native,
        Self::RiWork,
        Self::Catppuccin,
        Self::TokyoNight,
        Self::GruvboxLight,
        Self::Hermes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ghostty => "Follow Ghostty",
            Self::Native => "Native",
            Self::RiWork => "RiWork",
            Self::Catppuccin => "Catppuccin Mocha",
            Self::TokyoNight => "Tokyo Night",
            Self::GruvboxLight => "Gruvbox Light",
            Self::Hermes => "Hermes",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Ghostty => {
                "Use your Ghostty colors throughout RiWork. Configuration changes sync automatically."
            }
            Self::Native => {
                "Black on white, or white on black when macOS is dark. Follows the system appearance."
            }
            Self::RiWork => "RiWork's original dark palette with cyan and purple accents.",
            Self::Catppuccin => "A soft dark palette with pastel accents.",
            Self::TokyoNight => "A cool dark palette inspired by Tokyo at night.",
            Self::GruvboxLight => "A warm light palette with earthy accents.",
            Self::Hermes => "Deep cobalt, navy panels, warm cream prose and gold accents.",
        }
    }
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
    /// The keyboard focus ring of a control. Not one of the published colors:
    /// it is `gold` in the colorful themes and the primary color in Native,
    /// whose one signal color is kept for state that must stand out.
    pub focus: u32,
    /// What says an agent is working. `cyan` in the colorful themes, the signal
    /// color (`gold`) in Native. Not published either.
    pub working: u32,
    /// How a pane's tabs show which one is selected: the colorful themes draw
    /// terminal-style cells with a primary underline; Native fills the selected
    /// full-height cell with the content's own background, like a macOS tab bar.
    pub plain_tabs: bool,
    /// Whether the window controls sit on a filled island. Native leaves them on
    /// the window's own surface, as a Mac app does; the space stays reserved.
    pub controls_island: bool,
}

impl Palette {
    /// Hermes keeps the canvas distinct from its navy controls and user messages.
    /// Primary actions use warm gold; metadata stays subdued and readable.
    pub const HERMES: Self = Self {
        bg: 0x162564,
        panel: 0x192248,
        panel_active: 0x243364,
        divider: 0x2b4070,
        cyan: 0xf5dbb8,
        magenta: 0xb7bfd7,
        gold: 0xf5dbb8,
        text: 0xecdcc7,
        muted: 0xa1a7bb,
        focus: 0xf5dbb8,
        working: 0xf5dbb8,
        plain_tabs: false,
        controls_island: false,
    };

    pub const RIWORK: Self = Self {
        bg: 0x090d14,
        panel: 0x101720,
        panel_active: 0x14212a,
        divider: 0x253c45,
        cyan: 0x55e6dc,
        magenta: 0xce78ef,
        gold: 0xf4bf75,
        text: 0xd3e1e6,
        muted: 0x708993,
        focus: 0xf4bf75,
        working: 0x55e6dc,
        plain_tabs: false,
        controls_island: true,
    };

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
        plain_tabs: true,
        controls_island: false,
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
        plain_tabs: true,
        controls_island: false,
    };

    pub const fn native(dark: bool) -> Self {
        if dark {
            Self::NATIVE_DARK
        } else {
            Self::NATIVE_LIGHT
        }
    }

    pub fn from_terminal(theme: &TerminalTheme) -> Self {
        let bg = theme.background;
        let text = readable(theme.foreground, bg);
        let panel = mix(bg, text, 0.035);
        let panel_active = mix(bg, text, 0.075);
        let cyan = readable(theme.palette[6], panel_active);
        let gold = readable(theme.palette[3], panel_active);
        Self {
            bg,
            panel,
            panel_active,
            divider: mix(bg, text, 0.20),
            cyan,
            magenta: readable(theme.palette[5], panel_active),
            gold,
            text: readable(text, panel_active),
            muted: readable(mix(bg, text, 0.58), panel_active),
            focus: gold,
            working: cyan,
            plain_tabs: false,
            controls_island: true,
        }
    }
}

/// Phase 1 resolver: Ghostty configuration parsing and publishing are deferred.
pub const GHOSTTY_UNAVAILABLE: &str =
    "Following Ghostty is unavailable in this build; using the RiWork palette";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Appearance {
    pub selected: ThemeChoice,
    pub palette: Palette,
    pub terminal: Option<TerminalTheme>,
    pub ghostty: Option<TerminalTheme>,
    pub error: Option<String>,
}

impl Appearance {
    pub fn resolve(choice: ThemeChoice, system_dark: bool) -> Self {
        let terminal = match choice {
            ThemeChoice::Ghostty => None,
            ThemeChoice::Native => Some(native_terminal_theme(system_dark)),
            _ => Some(preset(choice)),
        };
        let palette = match choice {
            ThemeChoice::Ghostty | ThemeChoice::RiWork => Palette::RIWORK,
            ThemeChoice::Native => Palette::native(system_dark),
            ThemeChoice::Hermes => Palette::HERMES,
            _ => Palette::from_terminal(&terminal.expect("presets have terminal colors")),
        };
        Self {
            selected: choice,
            palette,
            terminal,
            ghostty: None,
            error: (choice == ThemeChoice::Ghostty).then(|| GHOSTTY_UNAVAILABLE.to_owned()),
        }
    }

    pub fn terminal_override(&self, force: bool) -> Option<TerminalTheme> {
        if self.selected == ThemeChoice::Native {
            return self.terminal.filter(|_| force);
        }
        self.terminal.or_else(|| force.then(riwork_terminal_theme))
    }

    pub fn shown_terminal(&self, force: bool) -> Option<TerminalTheme> {
        self.terminal_override(force).or(self.ghostty)
    }

    pub fn is_stale_for(&self, system_dark: bool) -> bool {
        self.selected == ThemeChoice::Native && self.palette != Palette::native(system_dark)
    }

    pub fn diff_colors(&self, force: bool) -> DiffColors {
        DiffColors::from_terminal(
            &self
                .shown_terminal(force)
                .unwrap_or_else(riwork_terminal_theme),
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

pub fn riwork_terminal_theme() -> TerminalTheme {
    terminal_theme(
        0x090d14,
        0xd3e1e6,
        [
            0x131b25, 0xf0738b, 0x61d5ae, 0xf4bf75, 0x78a9ff, 0xce78ef, 0x55e6dc, 0xd3e1e6,
            0x58707b, 0xff8ba0, 0x83ebc3, 0xffd191, 0x9bc0ff, 0xdfa3f7, 0x84f3ea, 0xffffff,
        ],
    )
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

fn preset(choice: ThemeChoice) -> TerminalTheme {
    match choice {
        ThemeChoice::Ghostty | ThemeChoice::RiWork => riwork_terminal_theme(),
        ThemeChoice::Native => native_terminal_theme(false),
        ThemeChoice::Hermes => terminal_theme(
            Palette::HERMES.bg,
            Palette::HERMES.text,
            [
                0x192248, 0xf29b9f, 0x9bc9ad, 0xf5dbb8, 0x9ebcf5, 0xc4b4df, 0xa8c8da, 0xecdcc7,
                0xa1a7bb, 0xffb7b8, 0xb4dfc1, 0xffe6c9, 0xb7ceff, 0xdfcaf2, 0xc2dfed, 0xfff1df,
            ],
        ),
        ThemeChoice::Catppuccin => terminal_theme(
            0x1e1e2e,
            0xcdd6f4,
            [
                0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
                0x585b70, 0xf37799, 0x89d88b, 0xebd391, 0x74a8fc, 0xf2aede, 0x6bd7ca, 0xbac2de,
            ],
        ),
        ThemeChoice::TokyoNight => terminal_theme(
            0x1a1b26,
            0xc0caf5,
            [
                0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
                0x414868, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
            ],
        ),
        ThemeChoice::GruvboxLight => terminal_theme(
            0xfbf1c7,
            0x3c3836,
            [
                0xfbf1c7, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0x7c6f64,
                0x928374, 0x9d0006, 0x79740e, 0xb57614, 0x076678, 0x8f3f71, 0x427b58, 0x3c3836,
            ],
        ),
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
    fn hermes_settings_round_trip_preserves_every_existing_theme_name() {
        for (name, choice) in [
            ("ghostty", ThemeChoice::Ghostty),
            ("native", ThemeChoice::Native),
            ("ri_work", ThemeChoice::RiWork),
            ("catppuccin", ThemeChoice::Catppuccin),
            ("tokyo_night", ThemeChoice::TokyoNight),
            ("gruvbox_light", ThemeChoice::GruvboxLight),
            ("hermes", ThemeChoice::Hermes),
        ] {
            let mut settings: Settings = serde_json::from_value(serde_json::json!({
                "theme": name, "chat_display_modes": {"saved-chat": "verbose"}
            }))
            .unwrap();
            assert_eq!(settings.theme, choice);
            settings.theme = ThemeChoice::Hermes;
            let saved = serde_json::to_value(&settings).unwrap();
            assert_eq!(saved["theme"], "hermes");
            assert_eq!(saved["chat_display_modes"]["saved-chat"], "verbose");
            settings.theme = choice;
            assert_eq!(serde_json::to_value(settings).unwrap()["theme"], name);
        }
        assert_eq!(ThemeChoice::default(), ThemeChoice::Ghostty);
    }

    #[test]
    fn terminal_override_is_what_terminals_are_forced_to() {
        let following = Appearance::resolve(ThemeChoice::Ghostty, false);
        assert_eq!(following.terminal_override(false), None);
        assert_eq!(
            following.terminal_override(true),
            Some(riwork_terminal_theme())
        );
        for theme in [
            ThemeChoice::RiWork,
            ThemeChoice::Catppuccin,
            ThemeChoice::TokyoNight,
            ThemeChoice::GruvboxLight,
            ThemeChoice::Hermes,
        ] {
            let selected = Appearance::resolve(theme, false);
            assert_eq!(selected.terminal_override(false), selected.terminal);
            assert_eq!(selected.terminal_override(true), selected.terminal);
        }
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
    }

    #[test]
    fn selected_theme_controls_terminals_and_keeps_legacy_preferences() {
        let mut settings = Settings::default();
        assert!(!settings.use_riwork_colors);
        assert!(settings.native_terminal_colors);
        for theme in ThemeChoice::ALL {
            settings.theme = theme;
            let appearance = Appearance::resolve(theme, true);
            assert_eq!(
                appearance
                    .terminal_override(settings.terminal_colors_forced())
                    .is_some(),
                theme != ThemeChoice::Ghostty
            );
        }
        settings.theme = ThemeChoice::Ghostty;
        settings.use_riwork_colors = true;
        assert_eq!(
            Appearance::resolve(settings.theme, false)
                .terminal_override(settings.terminal_colors_forced()),
            Some(riwork_terminal_theme())
        );
        settings.theme = ThemeChoice::Native;
        settings.native_terminal_colors = false;
        assert_eq!(
            Appearance::resolve(settings.theme, false)
                .terminal_override(settings.terminal_colors_forced()),
            None
        );
        assert!(settings.use_riwork_colors);
    }

    #[test]
    fn only_native_goes_stale_when_macos_switches_light_and_dark() {
        for choice in ThemeChoice::ALL {
            let light = Appearance::resolve(choice, false);
            let dark = Appearance::resolve(choice, true);
            assert!(!light.is_stale_for(false));
            assert!(!dark.is_stale_for(true));
            assert_eq!(light.is_stale_for(true), choice == ThemeChoice::Native);
            assert_eq!(dark.is_stale_for(false), choice == ThemeChoice::Native);
            if choice != ThemeChoice::Native {
                assert_eq!(light, dark);
            }
        }
    }

    /// The P1 successor inspects effective colors without an appearance file writer.
    #[test]
    fn published_terminal_follows_what_the_desktop_shows_while_following_ghostty() {
        let mut appearance = Appearance::resolve(ThemeChoice::Ghostty, false);
        let ghostty = terminal_theme(0x282c34, 0xabb2bf, [0x101010; 16]);
        appearance.ghostty = Some(ghostty);
        assert_eq!(appearance.shown_terminal(false), Some(ghostty));
        assert_eq!(
            appearance.shown_terminal(true),
            Some(riwork_terminal_theme())
        );
        let unknown = Appearance::resolve(ThemeChoice::Ghostty, false);
        assert_eq!(unknown.shown_terminal(false), None);
        assert_eq!(unknown.palette, Palette::RIWORK);
        assert!(unknown.error.is_some());
    }

    #[test]
    fn derived_palettes_nudge_text_to_wcag_contrast_and_diff_uses_ansi() {
        for choice in ThemeChoice::ALL {
            let appearance = Appearance::resolve(choice, false);
            for color in [
                appearance.palette.text,
                appearance.palette.muted,
                appearance.palette.cyan,
                appearance.palette.gold,
                appearance.palette.magenta,
            ] {
                if matches!(
                    choice,
                    ThemeChoice::Catppuccin | ThemeChoice::TokyoNight | ThemeChoice::GruvboxLight
                ) {
                    assert!(
                        contrast(color, appearance.palette.panel_active) >= 4.5,
                        "{choice:?}: {color:x}"
                    );
                }
            }
            let terminal = appearance.shown_terminal(true).unwrap();
            let diff = appearance.diff_colors(true);
            assert_eq!(
                diff.added,
                readable(terminal.palette[2], appearance.palette.panel_active)
            );
            assert_eq!(
                diff.removed,
                readable(terminal.palette[1], appearance.palette.panel_active)
            );
        }
        assert!(is_dark(0));
        assert!(!is_dark(0xffffff));
        assert!(is_dark(0xbbbbbb));
        assert!(!is_dark(0xbcbcbc));
    }
}
