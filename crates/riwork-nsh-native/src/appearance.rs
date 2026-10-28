//! Packed core palettes translated to CSS colors. Ghostty is parsed only by
//! the native terminal owner; this bridge reads its cached RGB projection.
use crate::{
    hub::Snapshot,
    service::{self, blocking},
    settings::theme_id,
};
use nsh_core::prelude::*;
use riwork_core::{
    appearance_file::{self, PaletteColors, Published, Rgb, TerminalColors},
    settings::Settings as CoreSettings,
    theme::{self, Palette, TerminalTheme},
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Mutex, OnceLock, RwLock, atomic::{AtomicBool, AtomicU64, Ordering}},
    time::{SystemTime, UNIX_EPOCH},
};

static GHOSTTY_THEME: OnceLock<RwLock<Result<TerminalTheme, String>>> = OnceLock::new();
static PUBLISH_ENABLED: AtomicBool = AtomicBool::new(false);
static RESOLVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static PUBLISHED: Mutex<(u64, Option<Published>)> = Mutex::new((0, None));

/// Only the production desktop host enables the phone's appearance document.
/// Merely loading the native registry in a preview or a fixture writes nothing.
pub fn enable_publication() {
    PUBLISH_ENABLED.store(true, Ordering::Release);
}

fn publish_saved(process: &service::ProcessService, requested: &str, system_dark: bool, sequence: u64) {
    if !PUBLISH_ENABLED.load(Ordering::Acquire) { return; }
    let mut sent = PUBLISHED.lock().unwrap_or_else(|error| error.into_inner());
    // A slower request for an older system appearance cannot replace a newer
    // window resolution that has already reached the phone document.
    if sequence < sent.0 { return; }
    let (settings, error) = process.preferences.core();
    // resolve() also serves temporary theme previews. Publish only the saved
    // choice, and never turn a failed settings read into a saved default.
    if error.is_some() || theme_id(settings.theme) != requested { return; }
    let appearance = theme::Appearance::resolve_with_ghostty(settings.theme, system_dark, ghostty_theme().ok());
    let palette = appearance.palette;
    let mut snapshot = Published::new(
        theme::is_dark(palette.bg),
        PaletteColors {
            bg: Rgb(palette.bg), panel: Rgb(palette.panel), panel_active: Rgb(palette.panel_active),
            divider: Rgb(palette.divider), cyan: Rgb(palette.cyan), magenta: Rgb(palette.magenta),
            gold: Rgb(palette.gold), text: Rgb(palette.text), muted: Rgb(palette.muted),
        },
        appearance.shown_terminal(settings.terminal_colors_forced()).map(|terminal| TerminalColors {
            background: Rgb(terminal.background), foreground: Rgb(terminal.foreground),
            palette: terminal.palette.map(Rgb),
        }),
    );
    snapshot.native = settings.theme == theme::ThemeChoice::Native;
    snapshot.mic = settings.dictation_mic;
    sent.0 = sequence;
    if sent.1.as_ref().is_some_and(|last| last.same_colors(&snapshot)) { return; }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    // The core publisher atomically replaces a private file and also dedupes
    // against a previous process. A failed write retries at the next change.
    match appearance_file::publish(process.home(), &snapshot, now) {
        Ok(_) => sent.1 = Some(snapshot),
        Err(error) => eprintln!("riwork appearance: {error}"),
    }
}

/// The terminal owner calls this after a main-thread parse. Resolving appearance
/// itself never dispatches to AppKit, including during process initialization.
pub fn set_ghostty_theme(value: Result<TerminalTheme, String>) {
    let cached = GHOSTTY_THEME.get_or_init(|| RwLock::new(value.clone()));
    *cached.write().unwrap_or_else(|error| error.into_inner()) = value;
    std::thread::spawn(|| {
        if let Ok(process) = service::initialized() {
            process.preferences.refresh();
        }
    });
}

fn ghostty_theme() -> Result<TerminalTheme, String> {
    GHOSTTY_THEME.get().map(|value| value.read().unwrap_or_else(|error| error.into_inner()).clone())
        .unwrap_or_else(|| Err("Ghostty configuration colors are not available in this host.".into()))
}

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

/// Colors shown by the settings picker, resolved from the same catalogue as
/// the workspace. Reading this catalogue never saves or publishes a choice.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceThemeDto {
    pub id: String,
    pub label: String,
    pub description: String,
    pub palette: PaletteDto,
    pub dark: bool,
    pub error: Option<String>,
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
        let ghostty = ghostty_theme();
        let appearance = theme::Appearance::resolve_with_ghostty(settings.theme, system_dark, ghostty.clone().ok());
        let ghostty_error = if settings.theme == theme::ThemeChoice::Ghostty {
            ghostty.err().map(|error| format!("{error} Showing RiWork's fallback colors until it can be read."))
        } else { None };
        let errors = [settings_error, ghostty_error].into_iter().flatten().collect::<Vec<_>>();
        let force = settings.terminal_colors_forced();
        let diff = appearance.diff_colors(force);
        Self {
            schema: 2,
            selected: theme_id(settings.theme),
            dark: theme::is_dark(appearance.palette.bg),
            palette: appearance.palette.into(),
            terminal_override: appearance.terminal_override(force).map(Into::into),
            error: (!errors.is_empty()).then(|| errors.join("\n")),
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
    /// Named color palettes plus Native and the cached Ghostty configuration.
    pub async fn catalog(system_dark: bool) -> Result<Vec<AppearanceThemeDto>, String> {
        let ghostty = ghostty_theme();
        Ok(theme::ThemeChoice::ALL.into_iter().map(|choice| {
            let resolved = theme::Appearance::resolve_with_ghostty(choice, system_dark, ghostty.clone().ok());
            AppearanceThemeDto {
                id: theme_id(choice),
                label: choice.label().into(),
                description: choice.description().into(),
                dark: theme::is_dark(resolved.palette.bg),
                palette: resolved.palette.into(),
                error: (choice == theme::ThemeChoice::Ghostty).then(|| ghostty.clone().err()).flatten(),
            }
        }).collect())
    }
    pub async fn get() -> Result<AppearanceDto, String> {
        let preferences = service::initialize().await?.preferences.clone();
        blocking(move || preferences.appearance(None, false)).await
    }
    /// Resolve for the caller's current host appearance. A per-window host
    /// event never overwrites another window's effective appearance.
    pub async fn resolve(theme: String, system_dark: bool) -> Result<AppearanceDto, String> {
        let sequence = RESOLVE_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1;
        let process = service::initialize().await?;
        blocking(move || {
            let resolved = process.preferences.appearance(Some(&theme), system_dark)?;
            publish_saved(&process, &theme, system_dark, sequence);
            Ok(resolved)
        }).await
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
