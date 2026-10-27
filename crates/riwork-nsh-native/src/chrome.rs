//! Window chrome's narrow appearance read. Domain IO stays on the blocking pool.
use nsh_core::prelude::*;
use riwork_core::{appearance_file, settings::SettingsStore, theme::ThemeChoice};
use serde_json::{Value, json};

#[module]
pub struct Appearance;

#[module]
impl Appearance {
    pub async fn get() -> Result<Value, String> {
        tokio::task::spawn_blocking(|| {
            let home = crate::service::process_home()?;
            let settings = SettingsStore::open(&home)?.load()?;
            let dark = std::env::var("RIWORK_APPEARANCE").as_deref() != Ok("light");
            let mut value = chrome_palette(settings.theme, dark);
            value["uiTextSize"] = json!(riwork_core::ui_text::effective_points(
                &settings,
                Some(riwork_core::ui_text::GHOSTTY_DEFAULT_FONT_SIZE)
            ));
            // Until the full appearance service lands, honour the already-resolved
            // Ghostty palette without parsing terminal configuration on the UI thread.
            if settings.theme == ThemeChoice::Ghostty {
                if let Some(published) = appearance_file::read(&home) {
                    value["panelActive"] = json!(published.palette.panel_active);
                    value["divider"] = json!(published.palette.divider);
                    value["background"] = json!(published.palette.bg);
                    value["panel"] = json!(published.palette.panel);
                    value["text"] = json!(published.palette.text);
                }
            }
            Ok(value)
        })
        .await
        .map_err(|error| format!("broken: {error}"))?
    }
}

fn mix(first: u32, second: u32, amount: f64) -> u32 {
    [16, 8, 0].into_iter().fold(0, |result, shift| {
        let a = f64::from((first >> shift) & 255);
        let b = f64::from((second >> shift) & 255);
        result | (((a + (b - a) * amount).round() as u32) << shift)
    })
}

fn chrome_palette(theme: ThemeChoice, dark: bool) -> Value {
    let (bg, text, panel, active, divider) = match theme {
        ThemeChoice::Native if dark => (0x000000, 0xf5f5f7, 0x1c1c1e, 0x2c2c2e, 0x3a3a3c),
        ThemeChoice::Native => (0xffffff, 0x1d1d1f, 0xf5f5f7, 0xe8e8ed, 0xd2d2d7),
        ThemeChoice::Hermes => (0x162564, 0xecdcc7, 0x192248, 0x243364, 0x2b4070),
        ThemeChoice::RiWork | ThemeChoice::Ghostty => {
            (0x090d14, 0xd3e1e6, 0x101720, 0x14212a, 0x253c45)
        }
        theme => {
            let (bg, text) = match theme {
                ThemeChoice::Catppuccin => (0x1e1e2e, 0xcdd6f4),
                ThemeChoice::TokyoNight => (0x1a1b26, 0xc0caf5),
                ThemeChoice::GruvboxLight => (0xfbf1c7, 0x3c3836),
                _ => unreachable!(),
            };
            (
                bg,
                text,
                mix(bg, text, 0.035),
                mix(bg, text, 0.075),
                mix(bg, text, 0.20),
            )
        }
    };
    json!({"theme":theme,"controlsIsland": !matches!(theme, ThemeChoice::Native | ThemeChoice::Hermes),
        "background":format!("#{bg:06x}"),"text":format!("#{text:06x}"),"panel":format!("#{panel:06x}"),
        "panelActive":format!("#{active:06x}"),"divider":format!("#{divider:06x}")})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_seven_themes_keep_native_and_hermes_islands_unpainted() {
        for theme in ThemeChoice::ALL {
            let palette = chrome_palette(theme, true);
            assert_eq!(
                palette["controlsIsland"],
                !matches!(theme, ThemeChoice::Native | ThemeChoice::Hermes)
            );
            assert!(palette["panelActive"].as_str().unwrap().starts_with('#'));
        }
        assert_eq!(
            chrome_palette(ThemeChoice::RiWork, true)["panelActive"],
            "#14212a"
        );
        assert_eq!(
            chrome_palette(ThemeChoice::Native, false)["background"],
            "#ffffff"
        );
    }
}
