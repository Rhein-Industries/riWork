//! Packed core palettes translated to CSS colors. Phase 1 never reads Ghostty
//! configuration and never publishes an appearance.json file. Terminal colors
//! that follow Ghostty are left to the native surface process, which owns that
//! configuration and its watcher.
use crate::{
    hub::Snapshot,
    service::{self, blocking},
    settings::theme_id,
};
use nsh_core::prelude::*;
use riwork_core::{
    settings::Settings as CoreSettings,
    theme::{self, Palette, TerminalTheme},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct PaletteDto {
    pub bg: String,
    pub panel: String,
    pub panel_active: String,
    pub divider: String,
    pub cyan: String,
    pub magenta: String,
    pub gold: String,
    pub text: String,
    pub muted: String,
    pub focus: String,
    pub working: String,
}

fn hex(color: u32) -> String {
    format!("#{color:06x}")
}
impl From<Palette> for PaletteDto {
    fn from(p: Palette) -> Self {
        Self {
            bg: hex(p.bg),
            panel: hex(p.panel),
            panel_active: hex(p.panel_active),
            divider: hex(p.divider),
            cyan: hex(p.cyan),
            magenta: hex(p.magenta),
            gold: hex(p.gold),
            text: hex(p.text),
            muted: hex(p.muted),
            focus: hex(p.focus),
            working: hex(p.working),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOverrideDto {
    pub source: String,
    pub background: String,
    pub foreground: String,
    pub palette: Vec<String>,
}
impl From<TerminalTheme> for TerminalOverrideDto {
    fn from(t: TerminalTheme) -> Self {
        Self {
            source: "override".into(),
            background: hex(t.background),
            foreground: hex(t.foreground),
            palette: t.palette.into_iter().map(hex).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct DiffColorsDto {
    pub added: String,
    pub removed: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceDto {
    pub schema: u32,
    pub selected: String,
    pub palette: PaletteDto,
    pub dark: bool,
    pub terminal_override: Option<TerminalOverrideDto>,
    pub error: Option<String>,
    pub diff: DiffColorsDto,
    /// Terminal-phase projection for both window schemes; `None` follows the
    /// terminal's own Ghostty configuration in the native surface process.
    pub terminal_light: Option<TerminalOverrideDto>,
    pub terminal_dark: Option<TerminalOverrideDto>,
    pub tab_close_behavior: String,
}

impl AppearanceDto {
    pub fn from_settings(
        settings: &CoreSettings,
        system_dark: bool,
        settings_error: Option<String>,
    ) -> Self {
        let appearance = theme::Appearance::resolve(settings.theme, system_dark);
        let force = settings.terminal_colors_forced();
        let diff = appearance.diff_colors(force);
        Self {
            schema: 2,
            selected: theme_id(settings.theme),
            dark: theme::is_dark(appearance.palette.bg),
            palette: appearance.palette.into(),
            terminal_override: appearance.terminal_override(force).map(Into::into),
            error: settings_error,
            diff: DiffColorsDto {
                added: hex(diff.added),
                removed: hex(diff.removed),
            },
            terminal_light: theme::Appearance::resolve(settings.theme, false)
                .terminal_override(force)
                .map(Into::into),
            terminal_dark: theme::Appearance::resolve(settings.theme, true)
                .terminal_override(force)
                .map(Into::into),
            tab_close_behavior: serde_json::to_value(settings.tab_close_behavior)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "ask".into()),
        }
    }
}

#[module]
pub struct Appearance;
#[module]
impl Appearance {
    pub async fn get() -> Result<AppearanceDto, String> {
        let preferences = service::initialize().await?.preferences.clone();
        blocking(move || preferences.appearance(None, false)).await
    }
    /// Resolve for the caller's current host appearance. A per-window host
    /// event never overwrites another window's effective appearance.
    pub async fn resolve(theme: String, system_dark: bool) -> Result<AppearanceDto, String> {
        let preferences = service::initialize().await?.preferences.clone();
        blocking(move || preferences.appearance(Some(&theme), system_dark)).await
    }
    pub fn subscribe(
        listener: Callback<Snapshot<AppearanceDto>>,
        cx: CallCx<'_>,
    ) -> Result<crate::Subscription, String> {
        Ok(crate::Subscription::start(cx, async move {
            if let Ok(service) = service::initialize().await {
                let preferences = service.preferences.clone();
                if let Ok(subscription) =
                    blocking(move || Ok(preferences.subscribe_appearance())).await
                {
                    crate::deliver(subscription, listener).await;
                }
            }
        }))
    }
}

use nsh_core::{
    JsonValue,
    element::{JsHost, JsIn, JsOwned, PropError, TsTypes},
};
macro_rules! snapshot_js_value {
    ($data:ty, $name:literal) => {
        impl JsValue for crate::hub::Snapshot<$data> {
            fn ts_type(types: &mut TsTypes) -> String {
                types.declare($name, |types| format!(
                    "export interface {} {{ kind: 'snapshot'; rev: number; data: {}; resync?: boolean; }}",
                    $name, <$data>::ts_type(types),
                ))
            }

            fn from_js(value: JsIn<'_>) -> std::result::Result<Self, PropError> {
                serde_json::from_value(JsonValue::from_js(value)?)
                    .map_err(|error| PropError(error.to_string()))
            }

            fn to_js<'a>(&self, host: &'a dyn JsHost) -> std::result::Result<JsOwned<'a>, PropError> {
                serde_json::to_value(self)
                    .map_err(|error| PropError(error.to_string()))?
                    .to_js(host)
            }
        }
    };
}

snapshot_js_value!(AppearanceDto, "AppearanceEvent");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_terminals_take_the_theme_or_follow_ghostty() {
        let mut settings = CoreSettings::default();
        assert!(settings.native_terminal_colors);
        let forced = AppearanceDto::from_settings(&settings, false, None);
        let dark = forced.terminal_dark.unwrap();
        assert_eq!(dark.source, "override");
        assert_eq!(dark.background, "#000000");
        assert_eq!(dark.palette.len(), 16);
        assert_eq!(forced.terminal_light.unwrap().background, "#ffffff");
        settings.native_terminal_colors = false;
        let following = AppearanceDto::from_settings(&settings, false, None);
        assert!(following.terminal_override.is_none());
        assert!(following.terminal_dark.is_none());
        assert!(following.terminal_light.is_none());
    }
}
