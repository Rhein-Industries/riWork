//! Global preferences adapted on the blocking pool; the process home watcher
//! calls `refresh`, so each window receives changes without a timer.
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};

use nsh_core::prelude::*;
use riwork_core::{
    settings::{Settings as CoreSettings, SettingsStore},
    theme::ThemeChoice,
    ui_text::{self, Face, InterfaceFont, SizeChange, TextPoints},
};
use serde::{Deserialize, Serialize};

use crate::{
    appearance::AppearanceDto,
    hub::{Hub, Snapshot, TopicSubscription},
    service::{self, blocking},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct ThemeChoiceDto {
    pub id: String,
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct TextStyleDto {
    pub points: f32,
    pub scale: f32,
    pub face: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct StatusBarItemDto {
    pub kind: String,
    pub label: String,
    pub enabled: bool,
    pub side: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct StatusBarDto {
    pub enabled: bool,
    pub usage_mode: String,
    pub items: Vec<StatusBarItemDto>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDto {
    pub schema: u32,
    pub theme: String,
    pub theme_choices: Vec<ThemeChoiceDto>,
    pub use_riwork_colors: bool,
    pub native_terminal_colors: bool,
    pub ui_text_size: f32,
    pub ui_text_matches_terminal: bool,
    pub interface_font: String,
    pub text: TextStyleDto,
    pub status_bar: StatusBarDto,
    pub error: Option<String>,
}

/// All fields are optional. Unknown fields and malformed choices are rejected
/// before reaching the durable store. A patch is applied under one core lock.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatch {
    pub theme: Option<String>,
    pub use_riwork_colors: Option<bool>,
    pub native_terminal_colors: Option<bool>,
    pub ui_text_size: Option<f32>,
    pub ui_text_matches_terminal: Option<bool>,
    pub interface_font: Option<String>,
    pub status_bar: Option<StatusBarDto>,
}

pub fn theme_id(choice: ThemeChoice) -> String {
    serde_json::to_value(choice)
        .expect("theme serializes")
        .as_str()
        .unwrap()
        .to_owned()
}

pub fn parse_theme(choice: &str) -> Result<ThemeChoice, String> {
    serde_json::from_value(serde_json::Value::String(choice.to_owned()))
        .map_err(|_| format!("invalid: unknown theme {choice:?}"))
}

impl SettingsDto {
    pub fn from_settings(settings: &CoreSettings, error: Option<String>) -> Self {
        let face = Face::of(settings);
        let points = ui_text::effective_points(settings, None)
            + face.offset(settings.ui_text_matches_terminal);
        let bar = settings.status_bar.normalized();
        Self {
            schema: 2,
            theme: theme_id(settings.theme),
            theme_choices: ThemeChoice::ALL
                .into_iter()
                .map(|choice| ThemeChoiceDto {
                    id: theme_id(choice),
                    label: choice.label().into(),
                    description: choice.description().into(),
                })
                .collect(),
            use_riwork_colors: settings.use_riwork_colors,
            native_terminal_colors: settings.native_terminal_colors,
            ui_text_size: settings.ui_text_size.points(),
            ui_text_matches_terminal: settings.ui_text_matches_terminal,
            interface_font: match settings.interface_font {
                InterfaceFont::System => "system",
                InterfaceFont::SystemMono => "system_mono",
            }
            .into(),
            text: TextStyleDto {
                points,
                scale: points / ui_text::REFERENCE_SIZE,
                face: match face {
                    Face::Menlo => "menlo",
                    Face::System => "system",
                    Face::SystemMono => "system_mono",
                    Face::Hermes => "hermes",
                }
                .into(),
            },
            status_bar: StatusBarDto {
                enabled: bar.enabled,
                usage_mode: serde_json::to_value(bar.usage_mode)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .into(),
                items: bar
                    .items
                    .into_iter()
                    .map(|item| StatusBarItemDto {
                        kind: serde_json::to_value(item.kind)
                            .unwrap()
                            .as_str()
                            .unwrap()
                            .into(),
                        label: item.kind.label().into(),
                        enabled: item.enabled,
                        side: match item.side {
                            riwork_core::status_bar::StatusSide::Left => "left",
                            riwork_core::status_bar::StatusSide::Right => "right",
                        }
                        .into(),
                    })
                    .collect(),
            },
            error,
        }
    }
}

struct ValidatedPatch {
    theme: Option<ThemeChoice>,
    use_riwork_colors: Option<bool>,
    native_terminal_colors: Option<bool>,
    ui_text_size: Option<TextPoints>,
    ui_text_matches_terminal: Option<bool>,
    interface_font: Option<InterfaceFont>,
    status_bar: Option<riwork_core::status_bar::StatusBarSettings>,
}

impl SettingsPatch {
    fn validate(&self) -> Result<ValidatedPatch, String> {
        if self.ui_text_size.is_some_and(|size| !size.is_finite()) {
            return Err("invalid: interface text size must be finite".into());
        }
        let interface_font = self
            .interface_font
            .as_ref()
            .map(|font| match font.as_str() {
                "system" => Ok(InterfaceFont::System),
                "system_mono" => Ok(InterfaceFont::SystemMono),
                _ => Err(format!("invalid: unknown interface font {font:?}")),
            })
            .transpose()?;
        let status_bar = self
            .status_bar
            .as_ref()
            .map(|bar| {
                use riwork_core::status_bar::{
                    StatusBarItem, StatusBarSettings, StatusItemKind, StatusSide, UsageMode,
                };
                let invalid = |error| format!("invalid: {error}");
                let usage_mode: UsageMode =
                    serde_json::from_value(serde_json::json!(bar.usage_mode)).map_err(invalid)?;
                let items = bar
                    .items
                    .iter()
                    .map(|item| {
                        let kind: StatusItemKind =
                            serde_json::from_value(serde_json::json!(item.kind))
                                .map_err(invalid)?;
                        let side: StatusSide = serde_json::from_value(serde_json::json!(item.side))
                            .map_err(invalid)?;
                        Ok(StatusBarItem {
                            kind,
                            side,
                            enabled: item.enabled,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Ok::<_, String>(
                    StatusBarSettings {
                        enabled: bar.enabled,
                        usage_mode,
                        items,
                    }
                    .normalized(),
                )
            })
            .transpose()?;
        Ok(ValidatedPatch {
            theme: self.theme.as_deref().map(parse_theme).transpose()?,
            use_riwork_colors: self.use_riwork_colors,
            native_terminal_colors: self.native_terminal_colors,
            ui_text_size: self.ui_text_size.map(TextPoints::new),
            ui_text_matches_terminal: self.ui_text_matches_terminal,
            interface_font,
            status_bar,
        })
    }
}

impl ValidatedPatch {
    fn apply(self, settings: &mut CoreSettings) {
        if let Some(value) = self.theme {
            settings.theme = value;
        }
        if let Some(value) = self.use_riwork_colors {
            settings.use_riwork_colors = value;
        }
        if let Some(value) = self.native_terminal_colors {
            settings.native_terminal_colors = value;
        }
        if let Some(value) = self.ui_text_size {
            settings.ui_text_size = value;
        }
        if let Some(value) = self.ui_text_matches_terminal {
            settings.ui_text_matches_terminal = value;
        }
        if let Some(value) = self.interface_font {
            settings.interface_font = value;
        }
        if let Some(value) = self.status_bar {
            settings.status_bar = value;
        }
    }
}

/// One durable status preference edit, always applied to the current document.
#[derive(Clone, Copy)]
pub enum StatusChange {
    Move {
        kind: riwork_core::status_bar::StatusItemKind,
        up: bool,
    },
    Side {
        kind: riwork_core::status_bar::StatusItemKind,
        side: riwork_core::status_bar::StatusSide,
    },
    Visible {
        kind: riwork_core::status_bar::StatusItemKind,
        visible: bool,
    },
    Enabled(bool),
}

/// Synchronous domain seam. Every call belongs on the bridge blocking pool.
pub trait SettingsBackend: Send + Sync + 'static {
    fn get(&self) -> Result<CoreSettings, String>;
    fn set(&self, patch: SettingsPatch) -> Result<CoreSettings, String>;
    fn change_text_size(&self, change: SizeChange) -> Result<CoreSettings, String>;
    fn change_status(&self, _change: StatusChange) -> Result<CoreSettings, String> {
        Err("unavailable: status preference edits are unavailable in this backend".into())
    }
}

pub struct CoreSettingsBackend {
    store: SettingsStore,
}
impl CoreSettingsBackend {
    pub fn new(home: PathBuf) -> Result<Self, String> {
        Ok(Self {
            store: SettingsStore::open(home)?,
        })
    }
}
impl SettingsBackend for CoreSettingsBackend {
    fn get(&self) -> Result<CoreSettings, String> {
        self.store.load().map_err(service::io_error)
    }
    fn set(&self, patch: SettingsPatch) -> Result<CoreSettings, String> {
        let selected = [
            (patch.theme.is_some(), "theme"),
            (patch.use_riwork_colors.is_some(), "use_riwork_colors"),
            (
                patch.native_terminal_colors.is_some(),
                "native_terminal_colors",
            ),
            (patch.ui_text_size.is_some(), "ui_text_size"),
            (
                patch.ui_text_matches_terminal.is_some(),
                "ui_text_matches_terminal",
            ),
            (patch.interface_font.is_some(), "interface_font"),
            (patch.status_bar.is_some(), "status_bar"),
        ]
        .into_iter()
        .filter_map(|(selected, key)| selected.then_some(key))
        .collect::<Vec<_>>();
        let patch = patch.validate()?;
        self.store
            .update_selected(&selected, move |settings| patch.apply(settings))
            .map_err(service::io_error)
    }
    fn change_text_size(&self, change: SizeChange) -> Result<CoreSettings, String> {
        self.store
            .update_selected(&["ui_text_size", "ui_text_matches_terminal"], |settings| {
                change.apply(settings, ui_text::GHOSTTY_DEFAULT_FONT_SIZE)
            })
            .map_err(service::io_error)
    }
    fn change_status(&self, change: StatusChange) -> Result<CoreSettings, String> {
        self.store
            .update_selected(&["status_bar"], |settings| match change {
                StatusChange::Move { kind, up } => {
                    settings.status_bar.move_item(kind, up);
                }
                StatusChange::Side { kind, side } => settings.status_bar.set_side(kind, side),
                StatusChange::Visible { kind, visible } => {
                    settings.status_bar.set_visible(kind, visible)
                }
                StatusChange::Enabled(enabled) => settings.status_bar.enabled = enabled,
            })
            .map_err(service::io_error)
    }
}

struct PreferenceState {
    settings: CoreSettings,
    error: Option<String>,
}

/// Shares the existing process watcher. No thread, watcher, poll, or timer is
/// created by this coordinator. Publication and mutations are serialized only
/// on background workers, never in synchronous module calls.
pub struct PreferenceService {
    backend: Arc<dyn SettingsBackend>,
    state: Mutex<PreferenceState>,
    settings_hub: Arc<Hub<SettingsDto>>,
    appearance_hub: Arc<Hub<AppearanceDto>>,
}

impl PreferenceService {
    pub fn new(home: PathBuf) -> anyhow::Result<Arc<Self>> {
        let backend = CoreSettingsBackend::new(home).map_err(anyhow::Error::msg)?;
        Ok(Self::with_backend(Arc::new(backend)))
    }
    pub fn with_backend(backend: Arc<dyn SettingsBackend>) -> Arc<Self> {
        let (settings, error) = match backend.get() {
            Ok(settings) => (settings, None),
            Err(error) => {
                eprintln!("RiWork settings load: {error}; using defaults");
                (CoreSettings::default(), Some(error))
            }
        };
        Arc::new(Self {
            settings_hub: Arc::new(Hub::new(SettingsDto::from_settings(
                &settings,
                error.clone(),
            ))),
            appearance_hub: Arc::new(Hub::new(AppearanceDto::from_settings(
                &settings,
                false,
                error.clone(),
            ))),
            state: Mutex::new(PreferenceState { settings, error }),
            backend,
        })
    }
    fn publish(&self, state: &PreferenceState) {
        self.settings_hub.publish(SettingsDto::from_settings(
            &state.settings,
            state.error.clone(),
        ));
        self.appearance_hub.publish(AppearanceDto::from_settings(
            &state.settings,
            false,
            state.error.clone(),
        ));
    }
    pub fn refresh(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match self.backend.get() {
            Ok(settings) => {
                state.settings = settings;
                state.error = None;
            }
            Err(error) => {
                state.error = Some(error);
            }
        }
        self.publish(&state);
    }
    pub fn get(&self) -> SettingsDto {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        SettingsDto::from_settings(&state.settings, state.error.clone())
    }
    pub fn set(&self, patch: SettingsPatch) -> Result<SettingsDto, String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.settings = self.backend.set(patch)?;
        state.error = None;
        self.publish(&state);
        Ok(SettingsDto::from_settings(&state.settings, None))
    }
    pub fn change_text_size(&self, action: &str) -> Result<SettingsDto, String> {
        let change = match action {
            "bigger" => SizeChange::Bigger,
            "smaller" => SizeChange::Smaller,
            "reset" => SizeChange::Reset,
            _ => return Err(format!("invalid: unknown text size action {action:?}")),
        };
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.settings = self.backend.change_text_size(change)?;
        state.error = None;
        self.publish(&state);
        Ok(SettingsDto::from_settings(&state.settings, None))
    }
    pub fn change_status(&self, change: StatusChange) -> Result<SettingsDto, String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.settings = self.backend.change_status(change)?;
        state.error = None;
        self.publish(&state);
        Ok(SettingsDto::from_settings(&state.settings, None))
    }
    pub fn move_item(&self, kind: &str, up: bool) -> Result<SettingsDto, String> {
        self.change_status(StatusChange::Move {
            kind: parse_status_kind(kind)?,
            up,
        })
    }
    pub fn set_side(&self, kind: &str, side: &str) -> Result<SettingsDto, String> {
        let side = serde_json::from_value(serde_json::json!(side))
            .map_err(|_| format!("invalid: unknown status side {side:?}"))?;
        self.change_status(StatusChange::Side {
            kind: parse_status_kind(kind)?,
            side,
        })
    }
    pub fn set_visible(&self, kind: &str, visible: bool) -> Result<SettingsDto, String> {
        self.change_status(StatusChange::Visible {
            kind: parse_status_kind(kind)?,
            visible,
        })
    }
    pub fn appearance(
        &self,
        theme: Option<&str>,
        system_dark: bool,
    ) -> Result<AppearanceDto, String> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut settings = state.settings.clone();
        if let Some(theme) = theme {
            settings.theme = parse_theme(theme)?;
        }
        Ok(AppearanceDto::from_settings(
            &settings,
            system_dark,
            state.error.clone(),
        ))
    }
    pub fn subscribe_settings(&self) -> TopicSubscription<SettingsDto> {
        self.settings_hub.subscribe()
    }
    pub fn subscribe_appearance(&self) -> TopicSubscription<AppearanceDto> {
        self.appearance_hub.subscribe()
    }
}

fn parse_status_kind(kind: &str) -> Result<riwork_core::status_bar::StatusItemKind, String> {
    serde_json::from_value(serde_json::json!(kind))
        .map_err(|_| format!("invalid: unknown status item {kind:?}"))
}

#[module]
pub struct Settings;
#[module]
impl Settings {
    pub async fn get() -> Result<SettingsDto, String> {
        let preferences = service::initialize().await?.preferences.clone();
        blocking(move || Ok(preferences.get())).await
    }
    pub async fn set(patch: SettingsPatch) -> Result<SettingsDto, String> {
        let preferences = service::initialize().await?.preferences.clone();
        blocking(move || preferences.set(patch)).await
    }
    pub async fn change_text_size(action: String) -> Result<SettingsDto, String> {
        let preferences = service::initialize().await?.preferences.clone();
        blocking(move || preferences.change_text_size(&action)).await
    }
    pub async fn move_item(kind: String, up: bool) -> Result<SettingsDto, String> {
        let preferences = service::initialize().await?.preferences.clone();
        blocking(move || preferences.move_item(&kind, up)).await
    }
    pub async fn set_side(kind: String, side: String) -> Result<SettingsDto, String> {
        let preferences = service::initialize().await?.preferences.clone();
        blocking(move || preferences.set_side(&kind, &side)).await
    }
    pub async fn set_visible(kind: String, visible: bool) -> Result<SettingsDto, String> {
        let preferences = service::initialize().await?.preferences.clone();
        blocking(move || preferences.set_visible(&kind, visible)).await
    }
    pub async fn set_enabled(enabled: bool) -> Result<SettingsDto, String> {
        let preferences = service::initialize().await?.preferences.clone();
        blocking(move || preferences.change_status(StatusChange::Enabled(enabled))).await
    }
    pub fn sentence_case(text: String) -> String {
        ui_text::sentence_case(&text)
    }
    pub fn cased(text: String, native: bool) -> String {
        if native { text } else { text.to_uppercase() }
    }
    pub fn subscribe(
        listener: Callback<Snapshot<SettingsDto>>,
        cx: CallCx<'_>,
    ) -> Result<crate::Subscription, String> {
        Ok(crate::Subscription::start(cx, async move {
            if let Ok(service) = service::initialize().await {
                let preferences = service.preferences.clone();
                if let Ok(subscription) =
                    blocking(move || Ok(preferences.subscribe_settings())).await
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

snapshot_js_value!(SettingsDto, "SettingsEvent");

// JsObject intentionally decodes only declared properties. Patches instead use
// the complete JSON object so serde can reject unknown caller-supplied keys.
impl JsValue for SettingsPatch {
    fn ts_type(types: &mut TsTypes) -> String {
        types.declare("SettingsPatch", |types| format!(
            "export interface SettingsPatch {{ theme?: string; useRiworkColors?: boolean; nativeTerminalColors?: boolean; uiTextSize?: number; uiTextMatchesTerminal?: boolean; interfaceFont?: 'system' | 'system_mono'; statusBar?: {}; }}",
            StatusBarDto::ts_type(types)))
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        serde_json::from_value(JsonValue::from_js(value)?)
            .map_err(|error| PropError(format!("invalid: {error}")))
    }
    fn to_js<'a>(&self, host: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        serde_json::to_value(self)
            .map_err(|error| PropError(error.to_string()))?
            .to_js(host)
    }
}
