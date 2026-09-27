//! Application palettes and colors resolved by the same Ghostty library as terminals.

use gpui::{App, Global};
use gpui_libghostty::{TerminalColor, TerminalTheme};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    Ghostty,
    RiWork,
    Catppuccin,
    TokyoNight,
    GruvboxLight,
}

impl ThemeChoice {
    pub const ALL: [Self; 5] = [
        Self::Ghostty,
        Self::RiWork,
        Self::Catppuccin,
        Self::TokyoNight,
        Self::GruvboxLight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Ghostty => "Follow Ghostty",
            Self::RiWork => "RiWork",
            Self::Catppuccin => "Catppuccin Mocha",
            Self::TokyoNight => "Tokyo Night",
            Self::GruvboxLight => "Gruvbox Light",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Ghostty => {
                "Use your Ghostty colors throughout RiWork. Configuration changes sync automatically."
            }
            Self::RiWork => "RiWork's original dark palette with cyan and purple accents.",
            Self::Catppuccin => "A soft dark palette with pastel accents.",
            Self::TokyoNight => "A cool dark palette inspired by Tokyo at night.",
            Self::GruvboxLight => "A warm light palette with earthy accents.",
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
}

impl Palette {
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
    };

    fn from_terminal(theme: &TerminalTheme) -> Self {
        let bg = color_u32(theme.background);
        let text = readable(color_u32(theme.foreground), bg);
        let panel = mix(bg, text, 0.035);
        let panel_active = mix(bg, text, 0.075);
        Self {
            bg,
            panel,
            panel_active,
            divider: mix(bg, text, 0.20),
            cyan: readable(color_u32(theme.palette[6]), panel_active),
            magenta: readable(color_u32(theme.palette[5]), panel_active),
            gold: readable(color_u32(theme.palette[3]), panel_active),
            text: readable(text, panel_active),
            muted: readable(mix(bg, text, 0.58), panel_active),
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
    /// Call on the application UI thread, never while creating or updating a surface.
    pub fn resolve(choice: ThemeChoice) -> Self {
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
            palette: if choice == ThemeChoice::RiWork {
                Palette::RIWORK
            } else {
                Palette::from_terminal(&theme)
            },
            terminal: Some(theme),
            ghostty: None,
            error: None,
        }
    }
}

pub fn palette(cx: &App) -> Palette {
    cx.try_global::<Appearance>()
        .map_or(Palette::RIWORK, |appearance| appearance.palette)
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

fn preset(choice: ThemeChoice) -> TerminalTheme {
    match choice {
        ThemeChoice::Ghostty | ThemeChoice::RiWork => riwork_terminal_theme(),
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

fn mix(first: u32, second: u32, amount: f64) -> u32 {
    let mut result = 0;
    for shift in [16, 8, 0] {
        let first = f64::from((first >> shift) & 255);
        let second = f64::from((second >> shift) & 255);
        result |= ((first + (second - first) * amount).round() as u32) << shift;
    }
    result
}

fn luminance(color: u32) -> f64 {
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

fn contrast(first: u32, second: u32) -> f64 {
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

    pub(super) fn read_config(
        path: Option<&std::path::Path>,
    ) -> Result<(TerminalTheme, Option<String>), String> {
        initialize();
        let config = Config(unsafe { ghostty_config_new() });
        if config.0.is_null() {
            return Err("Could not load Ghostty configuration".to_owned());
        }
        let mut background = Rgb::default();
        let mut foreground = Rgb::default();
        // ghostty_config_palette_s contains 256 RGB colors, not sixteen. Giving
        // config_get a sixteen-entry buffer would write beyond its bounds.
        let mut palette = [Rgb::default(); 256];
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
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod native {
    use super::*;
    pub fn read() -> Result<(TerminalTheme, Option<String>), String> {
        Err("Following Ghostty is unavailable on this platform".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_share_background_and_readable_colors_with_terminals() {
        for choice in [
            ThemeChoice::Catppuccin,
            ThemeChoice::TokyoNight,
            ThemeChoice::GruvboxLight,
        ] {
            let appearance = Appearance::resolve(choice);
            let terminal = appearance.terminal.unwrap();
            let palette = appearance.palette;
            assert_eq!(palette.bg, color_u32(terminal.background));
            for color in [
                palette.text,
                palette.muted,
                palette.cyan,
                palette.magenta,
                palette.gold,
            ] {
                assert!(
                    contrast(color, palette.panel_active) >= 4.5,
                    "{choice:?} {color:06x}"
                );
            }
        }
        assert!(luminance(Appearance::resolve(ThemeChoice::GruvboxLight).palette.bg) > 0.8);
        assert!(luminance(Appearance::resolve(ThemeChoice::TokyoNight).palette.bg) < 0.1);
    }

    #[test]
    fn repeated_native_reads_share_terminal_initialization() {
        let first = read_ghostty_theme().expect("linked Ghostty resolves colors");
        let second = read_ghostty_theme().expect("repeat reads do not reset Ghostty");
        assert_eq!(first, second);
    }
}
