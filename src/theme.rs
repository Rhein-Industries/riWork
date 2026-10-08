//! Application palettes and colors resolved by the same Ghostty library as terminals.

use crate::appearance_file::{PaletteColors, Published, Rgb, TerminalColors};
use crate::settings::Settings;
use gpui::{App, Global, WindowAppearance};
use gpui_libghostty::{TerminalColor, TerminalTheme};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

    fn from_terminal(theme: &TerminalTheme) -> Self {
        let bg = color_u32(theme.background);
        let text = readable(color_u32(theme.foreground), bg);
        let panel = mix(bg, text, 0.035);
        let panel_active = mix(bg, text, 0.075);
        let cyan = readable(color_u32(theme.palette[6]), panel_active);
        let gold = readable(color_u32(theme.palette[3]), panel_active);
        Self {
            bg,
            panel,
            panel_active,
            divider: mix(bg, text, 0.20),
            cyan,
            magenta: readable(color_u32(theme.palette[5]), panel_active),
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Appearance {
    pub selected: ThemeChoice,
    pub palette: Palette,
    /// Color overrides for the selected RiWork preset; absent when following Ghostty.
    pub terminal: Option<TerminalTheme>,
    /// All sixteen effective ANSI colors, including colors not used by the app UI.
    pub ghostty: Option<TerminalTheme>,
    pub error: Option<String>,
}

impl Global for Appearance {}

impl Appearance {
    /// Reloading uses Ghostty's parser, including named themes, includes, and overrides.
    /// Its conditional appearance matches the bundled terminal adapter's initial state.
    /// `system_dark` is macOS's light or dark appearance, which only Native follows.
    /// Call on the application UI thread, never while creating or updating a surface.
    pub fn resolve(choice: ThemeChoice, system_dark: bool) -> Self {
        if choice == ThemeChoice::Native {
            // Ghostty's own colors are what terminals keep with "Terminals match
            // the theme" off; unknown if the configuration cannot be read.
            return Self {
                selected: choice,
                palette: Palette::native(system_dark),
                terminal: Some(native_terminal_theme(system_dark)),
                ghostty: read_ghostty_theme().ok().map(|(theme, _)| theme),
                error: None,
            };
        }
        if choice == ThemeChoice::Ghostty {
            return match read_ghostty_theme() {
                Ok((theme, error)) => Self {
                    selected: choice,
                    palette: Palette::from_terminal(&theme),
                    terminal: None,
                    ghostty: Some(theme),
                    error,
                },
                Err(error) => Self {
                    selected: choice,
                    palette: Palette::RIWORK,
                    terminal: None,
                    ghostty: None,
                    error: Some(error),
                },
            };
        }
        let theme = preset(choice);
        Self {
            selected: choice,
            palette: match choice {
                ThemeChoice::RiWork => Palette::RIWORK,
                ThemeChoice::Hermes => Palette::HERMES,
                _ => Palette::from_terminal(&theme),
            },
            terminal: Some(theme),
            ghostty: None,
            error: None,
        }
    }

    /// The colors terminals are forced to: the selected preset's, RiWork's while
    /// following Ghostty with the RiWork terminal colors option on, or Native's
    /// while its "Terminals match the theme" option is on. `force` is whichever
    /// option the selected theme shows (`Settings::terminal_colors_forced`). None
    /// leaves terminals with the user's own Ghostty configuration.
    pub fn terminal_override(&self, force: bool) -> Option<TerminalTheme> {
        if self.selected == ThemeChoice::Native {
            return self.terminal.filter(|_| force);
        }
        self.terminal.or_else(|| force.then(riwork_terminal_theme))
    }

    /// Whether this is Native resolved for the other system appearance.
    pub fn is_stale_for(&self, system_dark: bool) -> bool {
        self.selected == ThemeChoice::Native && self.palette != Palette::native(system_dark)
    }

    /// The terminal colors the desktop actually shows, if they are known.
    pub fn shown_terminal(&self, force: bool) -> Option<TerminalTheme> {
        self.terminal_override(force).or(self.ghostty)
    }

    /// What the phone companion mirrors, not yet stamped with a time.
    pub fn published(&self, force: bool) -> Published {
        let palette = self.palette;
        let mut published = Published::new(
            is_dark(palette.bg),
            PaletteColors {
                bg: Rgb(palette.bg),
                panel: Rgb(palette.panel),
                panel_active: Rgb(palette.panel_active),
                divider: Rgb(palette.divider),
                cyan: Rgb(palette.cyan),
                magenta: Rgb(palette.magenta),
                gold: Rgb(palette.gold),
                text: Rgb(palette.text),
                muted: Rgb(palette.muted),
            },
            self.shown_terminal(force).map(|theme| TerminalColors {
                background: Rgb(color_u32(theme.background)),
                foreground: Rgb(color_u32(theme.foreground)),
                palette: theme.palette.map(|color| Rgb(color_u32(color))),
            }),
        );
        published.native = self.selected == ThemeChoice::Native;
        published
    }
}

/// Resolves the appearance for `choice` again, or returns None when following
/// Ghostty and none of the files it reads changed since the last successful
/// parse. Building, loading and freeing a Ghostty configuration takes the UI
/// thread, so the two-second poll must not do it for an unchanged config.
pub fn refresh_appearance(choice: ThemeChoice, cx: &mut App) -> Option<Appearance> {
    if choice != ThemeChoice::Ghostty {
        return Some(Appearance::resolve(choice, system_is_dark(cx)));
    }
    // Taken before parsing, so an edit made during the parse shows up next poll.
    let stamp = ghostty_config_stamp();
    let following = cx
        .try_global::<Appearance>()
        .is_some_and(|appearance| appearance.selected == choice && appearance.ghostty.is_some());
    if following && cx.default_global::<GhosttyWatch>().stamp.as_ref() == Some(&stamp) {
        return None;
    }
    let appearance = Appearance::resolve(choice, system_is_dark(cx));
    // A hard failure is retried on every poll; diagnostics about a config that
    // still produced colors are stable until the files change.
    cx.set_global(GhosttyWatch {
        stamp: appearance.ghostty.is_some().then_some(stamp),
    });
    Some(appearance)
}

/// The config files behind the last successful "Follow Ghostty" parse.
#[derive(Default)]
struct GhosttyWatch {
    stamp: Option<GhosttyConfigStamp>,
}

impl Global for GhosttyWatch {}

/// On-disk state of one file the Ghostty loader may read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
}

/// State of every file that can feed Ghostty's effective colors: the default
/// config files, the `config-file` includes they pull in, and the theme files
/// they name. A file that does not exist yet is part of the stamp too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GhosttyConfigStamp(Vec<(PathBuf, Option<FileStamp>)>);

pub fn ghostty_config_stamp() -> GhosttyConfigStamp {
    config_files::stamp()
}

/// Whether macOS shows dark mode: the application's effective appearance, which
/// follows the system unless RiWork forces one (see `force_system_appearance`).
pub fn system_is_dark(cx: &App) -> bool {
    matches!(
        cx.window_appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// `RIWORK_APPEARANCE=light` or `dark` pins the application's appearance, so the
/// Native theme's other mode can be looked at without changing the system
/// setting. Anything else, or nothing, follows the system.
pub fn force_system_appearance(cx: &App) {
    let forced = match std::env::var("RIWORK_APPEARANCE").as_deref() {
        Ok("light") => WindowAppearance::Light,
        Ok("dark") => WindowAppearance::Dark,
        _ => return,
    };
    cx.set_window_appearance(Some(forced));
}

pub fn palette(cx: &App) -> Palette {
    cx.try_global::<Appearance>()
        .map_or(Palette::RIWORK, |appearance| appearance.palette)
}

/// The colors for what a change added and what it removed, taken from the
/// terminal's green and red so a diff reads as it does in the user's own tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffColors {
    pub added: u32,
    pub removed: u32,
}

impl DiffColors {
    /// The green and red (ANSI 2 and 1) of `theme`, nudged until they read on
    /// `background`.
    pub fn from_terminal(theme: &TerminalTheme, background: u32) -> Self {
        Self {
            added: readable(color_u32(theme.palette[2]), background),
            removed: readable(color_u32(theme.palette[1]), background),
        }
    }
}

/// The diff colors for the theme on screen, readable on the panels' highlight
/// color. A configuration that gave no terminal colors falls back to RiWork's.
pub fn diff_colors(cx: &App) -> DiffColors {
    let force = cx
        .try_global::<Settings>()
        .is_some_and(Settings::terminal_colors_forced);
    let appearance = cx.try_global::<Appearance>();
    let theme = appearance
        .and_then(|appearance| appearance.shown_terminal(force))
        .unwrap_or_else(riwork_terminal_theme);
    DiffColors::from_terminal(&theme, palette(cx).panel_active)
}

const fn color(value: u32) -> TerminalColor {
    TerminalColor::new((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

fn color_u32(value: TerminalColor) -> u32 {
    (u32::from(value.r) << 16) | (u32::from(value.g) << 8) | u32::from(value.b)
}

fn terminal_theme(background: u32, foreground: u32, palette: [u32; 16]) -> TerminalTheme {
    TerminalTheme::new(color(background), color(foreground), palette.map(color))
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

/// Read effective colors through the linked, pinned gpui-libghostty dependency.
/// Calls must be serialized on the application UI thread.
pub fn read_ghostty_theme() -> Result<(TerminalTheme, Option<String>), String> {
    native::read()
}

/// Ghostty's effective `font-size`, through the same loader as the colors: the
/// default config files, their `config-file` includes, last value wins, and
/// Ghostty's built-in default when none sets it. Same threading rule as colors.
pub fn read_ghostty_font_size() -> Result<f32, String> {
    native::read_font_size(None)
}

/// How Ghostty's `window-padding-balance` spreads the space around the cell grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaddingBalance {
    False,
    True,
    Equal,
}

/// The padding settings that decide where Ghostty puts its grid in a terminal, in points: the
/// start (left, top) and end (right, bottom) of each axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GhosttyPadding {
    pub x: (u32, u32),
    pub y: (u32, u32),
    pub balance: PaddingBalance,
}

impl GhosttyPadding {
    /// What Ghostty uses when its configuration says nothing.
    pub const DEFAULT: Self = Self {
        x: (2, 2),
        y: (2, 2),
        balance: PaddingBalance::False,
    };
}

/// Ghostty's padding settings, read from the same default files and includes as the stamp, with
/// the last value winning. Ghostty's C configuration API does not return these two (they are
/// not a plain number), so they are read from the text.
pub fn read_ghostty_padding() -> GhosttyPadding {
    config_files::padding()
}

#[cfg(unix)]
mod config_files {
    use super::{FileStamp, GhosttyConfigStamp, GhosttyPadding, PaddingBalance};
    use std::{
        collections::{HashSet, VecDeque},
        env, fs,
        os::unix::fs::MetadataExt,
        path::{Path, PathBuf},
    };

    /// Bounds the walk for a pathological or cyclic include chain.
    const MAX_FILES: usize = 64;
    const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
    const CONFIG_NAMES: [&str; 2] = ["config", "config.ghostty"];

    pub(super) struct Locations {
        home: Option<PathBuf>,
        /// Directories holding a default config file and a `themes` folder.
        config_dirs: Vec<PathBuf>,
        /// Ghostty's bundled resources; their `themes` folder is searched last.
        resources: Option<PathBuf>,
    }

    impl Locations {
        fn from_env() -> Self {
            let variable = |name| {
                env::var_os(name)
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
            };
            let home = variable("HOME");
            let mut config_dirs = Vec::new();
            if let Some(base) = variable("XDG_CONFIG_HOME")
                .or_else(|| home.as_ref().map(|home| home.join(".config")))
            {
                config_dirs.push(base.join("ghostty"));
            }
            #[cfg(target_os = "macos")]
            if let Some(home) = &home {
                config_dirs.push(home.join("Library/Application Support/com.mitchellh.ghostty"));
            }
            Self {
                home,
                config_dirs,
                resources: variable("GHOSTTY_RESOURCES_DIR"),
            }
        }

        fn theme_dirs(&self) -> Vec<PathBuf> {
            self.config_dirs
                .iter()
                .map(|dir| dir.join("themes"))
                .chain(self.resources.iter().map(|dir| dir.join("themes")))
                .collect()
        }

        /// Absolute paths, `~/`, and paths relative to the file naming them.
        fn resolve(&self, value: &str, relative_to: Option<&Path>) -> Option<PathBuf> {
            if let Some(rest) = value.strip_prefix("~/") {
                return Some(self.home.as_ref()?.join(rest));
            }
            let path = Path::new(value);
            if path.is_absolute() {
                Some(path.to_owned())
            } else {
                Some(relative_to?.join(path))
            }
        }

        /// Visit the `key = value` entries of the default config files and of the files they
        /// include, in the order Ghostty reads them, so that a later value wins. `config-file`
        /// lines are followed, not visited. Returns the files read.
        fn walk(&self, mut visit: impl FnMut(&str, &str)) -> Vec<PathBuf> {
            let mut queue: VecDeque<PathBuf> = self
                .config_dirs
                .iter()
                .flat_map(|dir| CONFIG_NAMES.iter().map(move |name| dir.join(name)))
                .collect();
            let mut seen = HashSet::new();
            let mut files = Vec::new();
            while let Some(path) = queue.pop_front() {
                if files.len() >= MAX_FILES || !seen.insert(path.clone()) {
                    continue;
                }
                files.push(path.clone());
                let Some(text) = read_config(&path) else {
                    continue;
                };
                for (key, value) in entries(&text) {
                    if key == "config-file" {
                        let value = value.strip_prefix('?').unwrap_or(value).trim_matches('"');
                        if let Some(include) = (!value.is_empty())
                            .then(|| self.resolve(value, path.parent()))
                            .flatten()
                        {
                            queue.push_back(include);
                        }
                    } else {
                        visit(key, value);
                    }
                }
            }
            files
        }

        pub(super) fn stamp(&self) -> GhosttyConfigStamp {
            let mut themes = Vec::new();
            let mut files = self.walk(|key, value| {
                if key == "theme" {
                    themes.extend(theme_names(value));
                }
            });
            let mut seen: HashSet<PathBuf> = files.iter().cloned().collect();
            let theme_dirs = self.theme_dirs();
            for name in themes.into_iter().take(MAX_FILES) {
                // A theme is a name searched in the theme folders, or a path.
                let candidates: Vec<PathBuf> = if name.starts_with('/') || name.starts_with("~/") {
                    self.resolve(&name, None).into_iter().collect()
                } else {
                    theme_dirs.iter().map(|dir| dir.join(&name)).collect()
                };
                for candidate in candidates {
                    if seen.insert(candidate.clone()) {
                        files.push(candidate);
                    }
                }
            }
            GhosttyConfigStamp(
                files
                    .into_iter()
                    .map(|path| {
                        let stamp = stamp_file(&path);
                        (path, stamp)
                    })
                    .collect(),
            )
        }

        pub(super) fn padding(&self) -> GhosttyPadding {
            let mut padding = GhosttyPadding::DEFAULT;
            self.walk(|key, value| match key {
                "window-padding-x" => padding.x = parse_padding(value).unwrap_or(padding.x),
                "window-padding-y" => padding.y = parse_padding(value).unwrap_or(padding.y),
                "window-padding-balance" => {
                    padding.balance = match value {
                        "true" => PaddingBalance::True,
                        "equal" => PaddingBalance::Equal,
                        "false" => PaddingBalance::False,
                        _ => padding.balance,
                    }
                }
                _ => {}
            });
            padding
        }
    }

    pub(super) fn stamp() -> GhosttyConfigStamp {
        Locations::from_env().stamp()
    }

    pub(super) fn padding() -> GhosttyPadding {
        Locations::from_env().padding()
    }

    fn stamp_file(path: &Path) -> Option<FileStamp> {
        // Follow links: dotfile managers usually link the config into place.
        let metadata = fs::metadata(path).ok()?;
        Some(FileStamp {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
        })
    }

    fn read_config(path: &Path) -> Option<String> {
        let metadata = fs::metadata(path).ok()?;
        if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
            return None;
        }
        Some(String::from_utf8_lossy(&fs::read(path).ok()?).into_owned())
    }

    /// `key = value` lines; Ghostty allows comments only on lines of their own.
    fn entries(text: &str) -> impl Iterator<Item = (&str, &str)> {
        text.lines().filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((key.trim(), value.trim().trim_matches('"')))
        })
    }

    /// `window-padding-x`'s value: one number for both sides, or `start,end`, in points.
    fn parse_padding(value: &str) -> Option<(u32, u32)> {
        let number = |text: &str| text.trim().parse::<u32>().ok();
        match value.split_once(',') {
            Some((start, end)) => Some((number(start)?, number(end)?)),
            None => number(value).map(|both| (both, both)),
        }
    }

    /// Both variants of a paired `light:name,dark:name` value count.
    fn theme_names(value: &str) -> Vec<String> {
        value
            .split(',')
            .map(|part| {
                let part = part.trim();
                let part = part
                    .strip_prefix("light:")
                    .or_else(|| part.strip_prefix("dark:"))
                    .unwrap_or(part);
                part.trim().trim_matches('"').to_owned()
            })
            .filter(|name| !name.is_empty())
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        struct Fixture(PathBuf);

        impl Fixture {
            fn new() -> Self {
                let root = std::env::temp_dir()
                    .join(format!("riwork-theme-stamp-{}", uuid::Uuid::new_v4()));
                fs::create_dir_all(root.join("xdg/ghostty/themes")).unwrap();
                fs::create_dir_all(root.join("home")).unwrap();
                Self(root)
            }

            fn locations(&self) -> Locations {
                Locations {
                    home: Some(self.0.join("home")),
                    config_dirs: vec![self.0.join("xdg/ghostty"), self.0.join("support")],
                    resources: Some(self.0.join("resources")),
                }
            }

            fn write(&self, relative: &str, contents: &str) -> PathBuf {
                let path = self.0.join(relative);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, contents).unwrap();
                path
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        #[test]
        fn stamp_is_stable_until_a_relevant_file_changes() {
            let fixture = Fixture::new();
            fixture.write(
                "xdg/ghostty/config",
                "# theme = commented\ntheme = \"light:day,dark:night\"\nconfig-file = ?extra.conf\nconfig-file = ~/dotfiles/more.conf\n",
            );
            fixture.write("xdg/ghostty/extra.conf", "palette = 1=#111111\n");
            fixture.write("home/dotfiles/more.conf", "background = #000000\n");
            fixture.write("xdg/ghostty/themes/day", "background = #ffffff\n");
            let locations = fixture.locations();
            let first = locations.stamp();
            assert_eq!(first, locations.stamp());

            // Every part of the chain is watched, in either theme variant.
            for relative in [
                "xdg/ghostty/config",
                "xdg/ghostty/extra.conf",
                "home/dotfiles/more.conf",
                "xdg/ghostty/themes/day",
            ] {
                let before = locations.stamp();
                let path = fixture.0.join(relative);
                let mut contents = fs::read_to_string(&path).unwrap();
                contents.push_str("# edited\n");
                fs::write(&path, contents).unwrap();
                assert_ne!(before, locations.stamp(), "{relative}");
            }

            // A theme file that appears later is noticed, wherever it lives.
            fixture.write(
                "xdg/ghostty/config",
                "theme = light:day,dark:night\nconfig-file = extra.conf\n",
            );
            let before = locations.stamp();
            fixture.write("resources/themes/night", "background = #000000\n");
            assert_ne!(before, locations.stamp());
        }

        #[test]
        fn stamp_ignores_unrelated_files_and_survives_include_cycles() {
            let fixture = Fixture::new();
            fixture.write("xdg/ghostty/config", "config-file = a.conf\n");
            fixture.write(
                "xdg/ghostty/a.conf",
                "config-file = config\nconfig-file = a.conf\n",
            );
            let unrelated = fixture.write("xdg/ghostty/notes.txt", "not a config\n");
            let locations = fixture.locations();
            let before = locations.stamp();
            fs::write(&unrelated, "still not a config, but longer\n").unwrap();
            assert_eq!(before, locations.stamp());

            fixture.write("xdg/ghostty/config.ghostty", "theme = x\n");
            assert_ne!(before, locations.stamp());
        }

        #[test]
        fn padding_is_ghosttys_default_until_the_configuration_says_otherwise() {
            let fixture = Fixture::new();
            let locations = fixture.locations();
            assert_eq!(locations.padding(), GhosttyPadding::DEFAULT);

            // One number sets both sides; two set start and end; an include is read after the
            // file that names it, so its value wins.
            fixture.write(
                "xdg/ghostty/config",
                "window-padding-x = 10\nwindow-padding-y = 4, 6\nwindow-padding-balance = true\n\
                 config-file = more.conf\n",
            );
            fixture.write("xdg/ghostty/more.conf", "window-padding-x = 8,12\n");
            assert_eq!(
                locations.padding(),
                GhosttyPadding {
                    x: (8, 12),
                    y: (4, 6),
                    balance: PaddingBalance::True,
                }
            );

            // A value Ghostty would reject leaves the earlier one in place.
            fixture.write(
                "xdg/ghostty/more.conf",
                "window-padding-x = wide\nwindow-padding-balance = equal\n",
            );
            let padding = locations.padding();
            assert_eq!(padding.x, (10, 10));
            assert_eq!(padding.balance, PaddingBalance::Equal);
        }
    }
}

#[cfg(not(unix))]
mod config_files {
    use super::{GhosttyConfigStamp, GhosttyPadding};

    pub(super) fn stamp() -> GhosttyConfigStamp {
        GhosttyConfigStamp::default()
    }

    pub(super) fn padding() -> GhosttyPadding {
        GhosttyPadding::DEFAULT
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod native {
    use super::*;
    use std::ffi::{CStr, c_char, c_void};

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rgb {
        r: u8,
        g: u8,
        b: u8,
    }

    #[repr(C)]
    struct Diagnostic {
        message: *const c_char,
    }

    unsafe extern "C" {
        fn ghostty_config_new() -> *mut c_void;
        fn ghostty_config_free(config: *mut c_void);
        fn ghostty_config_load_file(config: *mut c_void, path: *const c_char);
        fn ghostty_config_load_default_files(config: *mut c_void);
        fn ghostty_config_load_recursive_files(config: *mut c_void);
        fn ghostty_config_finalize(config: *mut c_void);
        fn ghostty_config_get(
            config: *mut c_void,
            value: *mut c_void,
            key: *const c_char,
            len: usize,
        ) -> bool;
        fn ghostty_config_diagnostics_count(config: *mut c_void) -> u32;
        fn ghostty_config_get_diagnostic(config: *mut c_void, index: u32) -> Diagnostic;
    }

    #[cfg(target_os = "macos")]
    unsafe extern "C" {
        fn gpui_ghostty_surface_new(
            parent: *mut c_void,
            directory: *const c_char,
            command: *const c_char,
            user_config: bool,
            theme_path: *const c_char,
            quiet: bool,
            userdata: *mut c_void,
            wakeup: unsafe extern "C" fn(*mut c_void),
            clipboard: unsafe extern "C" fn(*mut c_void, i32, *const c_char) -> bool,
        ) -> *mut c_void;
    }

    #[cfg(target_os = "linux")]
    unsafe extern "C" {
        fn gpui_ghostty_surface_linux_new(
            platform: *mut c_void,
            make_current: unsafe extern "C" fn(*mut c_void) -> bool,
            clear_current: unsafe extern "C" fn(*mut c_void),
            swap_buffers: unsafe extern "C" fn(*mut c_void),
            directory: *const c_char,
            command: *const c_char,
            user_config: bool,
            theme_path: *const c_char,
            quiet: bool,
            scale: f64,
            userdata: *mut c_void,
            wakeup: unsafe extern "C" fn(*mut c_void),
            clipboard: unsafe extern "C" fn(*mut c_void, i32, *const c_char) -> bool,
        ) -> *mut c_void;
    }

    unsafe extern "C" fn noop(_: *mut c_void) {}
    unsafe extern "C" fn deny(_: *mut c_void, _: i32, _: *const c_char) -> bool {
        false
    }
    #[cfg(target_os = "linux")]
    unsafe extern "C" fn no_context(_: *mut c_void) -> bool {
        false
    }

    fn initialize() {
        // Do not call ghostty_init directly: it resets process-global state. The
        // pinned 0.3.1 shim owns dispatch_once/pthread_once and initializes first,
        // then rejects a null parent before allocating a surface or using callbacks.
        // Sharing that gate also works if a terminal was created before this read.
        unsafe {
            #[cfg(target_os = "macos")]
            gpui_ghostty_surface_new(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                false,
                std::ptr::null(),
                false,
                std::ptr::null_mut(),
                noop,
                deny,
            );
            #[cfg(target_os = "linux")]
            gpui_ghostty_surface_linux_new(
                std::ptr::null_mut(),
                no_context,
                noop,
                noop,
                std::ptr::null(),
                std::ptr::null(),
                false,
                std::ptr::null(),
                false,
                1.0,
                std::ptr::null_mut(),
                noop,
                deny,
            );
        }
    }

    struct Config(*mut c_void);
    impl Drop for Config {
        fn drop(&mut self) {
            unsafe { ghostty_config_free(self.0) };
        }
    }

    pub fn read() -> Result<(TerminalTheme, Option<String>), String> {
        read_config(None)
    }

    /// The default config files, or `path`, with their `config-file` includes.
    fn load(path: Option<&std::path::Path>) -> Result<Config, String> {
        initialize();
        let config = Config(unsafe { ghostty_config_new() });
        if config.0.is_null() {
            return Err("Could not load Ghostty configuration".to_owned());
        }
        unsafe {
            if let Some(path) = path {
                use std::os::unix::ffi::OsStrExt;
                let path = std::ffi::CString::new(path.as_os_str().as_bytes())
                    .map_err(|_| "Ghostty config path contains a NUL byte".to_owned())?;
                ghostty_config_load_file(config.0, path.as_ptr());
            } else {
                ghostty_config_load_default_files(config.0);
            }
            ghostty_config_load_recursive_files(config.0);
            ghostty_config_finalize(config.0);
        }
        Ok(config)
    }

    /// The effective `font-size` in points: the last value set, or Ghostty's own default.
    pub(super) fn read_font_size(path: Option<&std::path::Path>) -> Result<f32, String> {
        let config = load(path)?;
        let mut size = 0.0_f32;
        // `font-size` is an f32 in Config.zig, which c_get writes as one.
        if !unsafe {
            ghostty_config_get(
                config.0,
                (&mut size as *mut f32).cast(),
                c"font-size".as_ptr(),
                9,
            )
        } {
            return Err("Ghostty did not provide its font size".to_owned());
        }
        Ok(size)
    }

    pub(super) fn read_config(
        path: Option<&std::path::Path>,
    ) -> Result<(TerminalTheme, Option<String>), String> {
        let config = load(path)?;
        let mut background = Rgb::default();
        let mut foreground = Rgb::default();
        // ghostty_config_palette_s contains 256 RGB colors, not sixteen. Giving
        // config_get a sixteen-entry buffer would write beyond its bounds.
        let mut palette = [Rgb::default(); 256];
        unsafe {
            if !ghostty_config_get(
                config.0,
                (&mut background as *mut Rgb).cast(),
                c"background".as_ptr(),
                10,
            ) || !ghostty_config_get(
                config.0,
                (&mut foreground as *mut Rgb).cast(),
                c"foreground".as_ptr(),
                10,
            ) || !ghostty_config_get(
                config.0,
                palette.as_mut_ptr().cast(),
                c"palette".as_ptr(),
                7,
            ) {
                return Err("Ghostty did not provide its effective colors".to_owned());
            }
        }
        let count = unsafe { ghostty_config_diagnostics_count(config.0) };
        let error = if count == 0 {
            None
        } else {
            let mut messages = Vec::new();
            for index in 0..count.min(3) {
                let diagnostic = unsafe { ghostty_config_get_diagnostic(config.0, index) };
                if !diagnostic.message.is_null() {
                    messages.push(
                        unsafe { CStr::from_ptr(diagnostic.message) }
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
            Some(format!("Ghostty configuration: {}", messages.join("; ")))
        };
        let convert = |rgb: Rgb| TerminalColor::new(rgb.r, rgb.g, rgb.b);
        Ok((
            TerminalTheme::new(
                convert(background),
                convert(foreground),
                std::array::from_fn(|index| convert(palette[index])),
            ),
            error,
        ))
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod native_tests {
    use super::*;

    #[test]
    fn native_parser_resolves_themes_includes_overrides_and_reload() {
        let directory = std::env::temp_dir().join(format!("riwork-theme-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let theme = directory.join("test-theme");
        let include = directory.join("included.ghostty");
        let config = directory.join("config.ghostty");
        std::fs::write(&theme, "background = #121314\nforeground = #dedfe0\npalette = 3=#112233\npalette = 6=#445566\n").unwrap();
        std::fs::write(&include, "palette = 6=#778899\n").unwrap();
        std::fs::write(
            &config,
            format!(
                "theme = {}\nconfig-file = {}\nbackground = #212223\n",
                theme.display(),
                include.display()
            ),
        )
        .unwrap();
        let (first, error) = native::read_config(Some(&config)).unwrap();
        assert_eq!(error, None);
        assert_eq!(first.background, color(0x212223));
        assert_eq!(first.foreground, color(0xdedfe0));
        assert_eq!(first.palette[3], color(0x112233));
        assert_eq!(first.palette[6], color(0x778899));
        std::fs::write(&include, "palette = 6=#abcdef\n").unwrap();
        let (second, error) = native::read_config(Some(&config)).unwrap();
        assert_eq!(error, None);
        assert_eq!(second.palette[6], color(0xabcdef));
        assert_eq!(second.background, first.background);
        assert_ne!(first, second);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn native_font_size_follows_includes_and_falls_back_to_ghosttys_default() {
        let directory =
            std::env::temp_dir().join(format!("riwork-font-size-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let config = directory.join("config.ghostty");
        let include = directory.join("included.ghostty");
        std::fs::write(&config, "background = #000000\n").unwrap();
        assert_eq!(
            native::read_font_size(Some(&config)).unwrap(),
            crate::ui_text::GHOSTTY_DEFAULT_FONT_SIZE
        );
        // Includes load after the file naming them, so the include's value wins.
        std::fs::write(&include, "font-size = 15.5\n").unwrap();
        std::fs::write(
            &config,
            format!("font-size = 11\nconfig-file = {}\n", include.display()),
        )
        .unwrap();
        assert_eq!(native::read_font_size(Some(&config)).unwrap(), 15.5);
        std::fs::write(&config, "font-size = 11\nfont-size = 17\n").unwrap();
        assert_eq!(native::read_font_size(Some(&config)).unwrap(), 17.0);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod native {
    use super::*;
    pub fn read() -> Result<(TerminalTheme, Option<String>), String> {
        Err("Following Ghostty is unavailable on this platform".to_owned())
    }
    pub(super) fn read_font_size(_: Option<&std::path::Path>) -> Result<f32, String> {
        Err("Reading Ghostty's font size is unavailable on this platform".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                "theme": name,
                "chat_display_modes": { "saved-chat": "verbose" }
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

    fn following_ghostty(theme: Option<TerminalTheme>) -> Appearance {
        Appearance {
            selected: ThemeChoice::Ghostty,
            palette: theme
                .as_ref()
                .map_or(Palette::RIWORK, Palette::from_terminal),
            terminal: None,
            ghostty: theme,
            error: None,
        }
    }

    fn rgb(theme: &TerminalTheme) -> TerminalColors {
        TerminalColors {
            background: Rgb(color_u32(theme.background)),
            foreground: Rgb(color_u32(theme.foreground)),
            palette: theme.palette.map(|color| Rgb(color_u32(color))),
        }
    }

    #[test]
    fn published_terminal_follows_what_the_desktop_shows_while_following_ghostty() {
        let ghostty = terminal_theme(
            0x282c34,
            0xabb2bf,
            std::array::from_fn(|index| 0x101010 * (index as u32 % 15) + index as u32),
        );
        let appearance = following_ghostty(Some(ghostty));
        // Ghostty's own colors, unless the RiWork terminal colors option is on.
        assert_eq!(appearance.shown_terminal(false), Some(ghostty));
        assert_eq!(appearance.published(false).terminal, Some(rgb(&ghostty)));
        assert_eq!(
            appearance.shown_terminal(true),
            Some(riwork_terminal_theme())
        );
        assert_eq!(
            appearance.published(true).terminal,
            Some(rgb(&riwork_terminal_theme()))
        );
        // The application palette is Ghostty's either way.
        assert_eq!(
            appearance.published(true).palette,
            appearance.published(false).palette
        );
        assert_eq!(appearance.published(false).palette.bg.hex(), "#282c34");
        assert!(appearance.published(false).dark);

        // A configuration that could not be read: the palette falls back to
        // RiWork's and the terminal colors are unknown, unless the option forces them.
        let failed = following_ghostty(None);
        assert_eq!(failed.published(false).terminal, None);
        assert_eq!(failed.published(false).palette.bg.hex(), "#090d14");
        assert_eq!(
            failed.published(true).terminal,
            Some(rgb(&riwork_terminal_theme()))
        );

        let light = following_ghostty(Some(terminal_theme(0xfdf6e3, 0x657b83, [0x657b83; 16])));
        assert!(!light.published(false).dark);
    }

    #[test]
    fn terminal_override_is_what_terminals_are_forced_to() {
        let riwork = riwork_terminal_theme();
        let following = following_ghostty(Some(riwork));
        assert_eq!(following.terminal_override(false), None);
        assert_eq!(following.terminal_override(true), Some(riwork));
        let gruvbox = Appearance::resolve(ThemeChoice::GruvboxLight, false);
        assert_eq!(gruvbox.terminal_override(false), gruvbox.terminal);
        assert_eq!(gruvbox.terminal_override(true), gruvbox.terminal);
    }

    #[test]
    fn native_terminals_match_the_theme_only_while_the_option_is_on() {
        let ghostty = terminal_theme(0x282c34, 0xabb2bf, [0x101010; 16]);
        let mut appearance = Appearance::resolve(ThemeChoice::Native, true);
        appearance.ghostty = Some(ghostty);
        let native = native_terminal_theme(true);
        assert_eq!(appearance.terminal_override(true), Some(native));
        assert_eq!(appearance.published(true).terminal, Some(rgb(&native)));
        // Off keeps Ghostty's own colors: no override, and that is what the phone shows.
        assert_eq!(appearance.terminal_override(false), None);
        assert_eq!(appearance.published(false).terminal, Some(rgb(&ghostty)));
        // The palette is Native's either way.
        assert_eq!(
            appearance.published(false).palette,
            appearance.published(true).palette
        );
    }

    #[test]
    fn only_native_goes_stale_when_macos_switches_light_and_dark() {
        let light = Appearance::resolve(ThemeChoice::Native, false);
        assert!(!light.is_stale_for(false));
        assert!(light.is_stale_for(true));
        let dark = Appearance::resolve(ThemeChoice::Native, true);
        assert!(dark.is_stale_for(false));
        assert!(!dark.is_stale_for(true));
        for choice in [ThemeChoice::RiWork, ThemeChoice::GruvboxLight] {
            let appearance = Appearance::resolve(choice, false);
            assert!(!appearance.is_stale_for(true));
            assert_eq!(appearance, Appearance::resolve(choice, true));
        }
    }

    #[test]
    fn repeated_native_reads_share_terminal_initialization() {
        let first = read_ghostty_theme().expect("linked Ghostty resolves colors");
        let second = read_ghostty_theme().expect("repeat reads do not reset Ghostty");
        assert_eq!(first, second);
    }
}
