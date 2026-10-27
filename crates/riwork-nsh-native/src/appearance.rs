//! Terminal color projection from core Settings, with no renderer dependency.
//! Preset values preserve the desktop theme constants. Ghostty parsing remains
//! in the native surface process, where its config and file watcher are owned.
use nsh_core::prelude::*;
use riwork_core::{
    settings::{Settings, SettingsStore},
    theme::ThemeChoice,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceSnapshot {
    pub schema: u32,
    pub theme: String,
    pub terminal_light: Value,
    pub terminal_dark: Value,
    pub tab_close_behavior: String,
}

pub fn load(home: &Path) -> Result<AppearanceSnapshot, String> {
    let settings = SettingsStore::open(home)?.load()?;
    Ok(resolve(&settings))
}

pub fn resolve(settings: &Settings) -> AppearanceSnapshot {
    let terminal = |dark| {
        let follows_ghostty = (settings.theme == ThemeChoice::Ghostty
            || settings.theme == ThemeChoice::Native)
            && !settings.terminal_colors_forced();
        if follows_ghostty {
            json!({ "source": "ghostty" })
        } else if settings.theme == ThemeChoice::Native {
            native_terminal_theme(dark)
        } else {
            preset(settings.theme)
        }
    };
    AppearanceSnapshot {
        schema: 1,
        theme: serde_json::to_value(settings.theme)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned(),
        terminal_light: terminal(false),
        terminal_dark: terminal(true),
        tab_close_behavior: serde_json::to_value(settings.tab_close_behavior)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned(),
    }
}

fn terminal_theme(background: u32, foreground: u32, palette: [u32; 16]) -> Value {
    let color = |value: u32| format!("#{value:06x}");
    json!({ "source": "override", "background": color(background), "foreground": color(foreground),
        "palette": palette.map(color) })
}

pub fn riwork_terminal_theme() -> Value {
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
pub fn native_terminal_theme(dark: bool) -> Value {
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

fn preset(choice: ThemeChoice) -> Value {
    match choice {
        ThemeChoice::Ghostty | ThemeChoice::RiWork => riwork_terminal_theme(),
        ThemeChoice::Native => native_terminal_theme(false),
        ThemeChoice::Hermes => terminal_theme(
            0x162564,
            0xecdcc7,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_preset_preserves_sixteen_colors_and_ghostty_switching() {
        for theme in ThemeChoice::ALL {
            let mut settings = Settings {
                theme,
                ..Settings::default()
            };
            let projected = resolve(&settings);
            if theme != ThemeChoice::Ghostty {
                assert_eq!(projected.terminal_dark["source"], "override");
                assert_eq!(
                    projected.terminal_dark["palette"].as_array().unwrap().len(),
                    16
                );
            }
            if theme == ThemeChoice::Native {
                assert_eq!(projected.terminal_dark["background"], "#000000");
                assert_eq!(projected.terminal_light["background"], "#ffffff");
                settings.native_terminal_colors = false;
                assert_eq!(
                    resolve(&settings).terminal_dark,
                    json!({"source":"ghostty"})
                );
            }
            if theme == ThemeChoice::Ghostty {
                assert_eq!(projected.terminal_dark, json!({"source":"ghostty"}));
                settings.use_riwork_colors = true;
                assert_eq!(resolve(&settings).terminal_dark["background"], "#090d14");
            }
        }
    }
}
