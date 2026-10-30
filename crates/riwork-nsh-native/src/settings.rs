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

#[path = "settings_forms.rs"]
pub mod forms;
pub use forms::{DelegationDto, DelegationPatch, ModelChoiceDto, RouteChoiceDto, RouteDto};

use crate::{
    appearance::AppearanceDto,
    hub::{Hub, Snapshot, TopicSubscription},
    service::{self, blocking},
};

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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectOrderDto {
    pub sort: String,
    pub descending: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDto {
    pub schema: u32,
    pub theme: String,
    pub native_terminal_colors: bool,
    pub use_riwork_colors: bool,
    pub open_preview_on_select: bool,
    pub remember_window_size: bool,
    pub panel_tab_icons: bool,
    pub project_order: ProjectOrderDto,
    pub tab_close_behavior: String,
    pub agent_inline_mode: bool,
    pub agent_terminals_unrestricted: bool,
    pub dictation_mic: bool,
    /// The speech model dictation uses; Apple's for a key this build does not know.
    pub dictation_model: String,
    /// `right_command`, `right_option` or `off`.
    pub push_to_talk_key: String,
    pub claude_chat_ignores_api_key: bool,
    pub codex_subscription_balancing: bool,
    pub dashboard_haiku_summaries: bool,
    pub orchestrator_mode: String,
    pub orchestrator_chat_provider: String,
    pub orchestrator_chat_model: Option<String>,
    pub orchestrator_unreadable_note: Option<String>,
    pub model_choices: Vec<ModelChoiceDto>,
    pub delegation: DelegationDto,
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
    pub native_terminal_colors: Option<bool>,
    pub use_riwork_colors: Option<bool>,
    pub open_preview_on_select: Option<bool>,
    pub remember_window_size: Option<bool>,
    pub panel_tab_icons: Option<bool>,
    pub project_order: Option<ProjectOrderDto>,
    pub tab_close_behavior: Option<String>,
    pub agent_inline_mode: Option<bool>,
    pub agent_terminals_unrestricted: Option<bool>,
    pub dictation_mic: Option<bool>,
    pub dictation_model: Option<String>,
    pub push_to_talk_key: Option<String>,
    pub claude_chat_ignores_api_key: Option<bool>,
    pub codex_subscription_balancing: Option<bool>,
    pub dashboard_haiku_summaries: Option<bool>,
    pub orchestrator_mode: Option<String>,
    pub orchestrator_chat_provider: Option<String>,
    #[serde(
        default,
        deserialize_with = "forms::nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub orchestrator_chat_model: Option<Option<String>>,
    pub delegation_patch: Option<DelegationPatch>,
    pub ui_text_size: Option<f32>,
    pub ui_text_matches_terminal: Option<bool>,
    pub interface_font: Option<String>,
    pub status_bar: Option<StatusBarDto>,
}

pub fn theme_id(choice: ThemeChoice) -> String {
    choice.id().to_owned()
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
            native_terminal_colors: settings.native_terminal_colors,
            use_riwork_colors: settings.use_riwork_colors,
            open_preview_on_select: settings.open_preview_on_select,
            remember_window_size: settings.remember_window_size,
            panel_tab_icons: settings.panel_tab_icons,
            project_order: ProjectOrderDto {
                sort: serde_json::to_value(settings.project_order.by).unwrap().as_str().unwrap().into(),
                descending: settings.project_order.descending,
            },
            tab_close_behavior: serde_json::to_value(settings.tab_close_behavior)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            agent_inline_mode: settings.agent_inline_mode,
            agent_terminals_unrestricted: settings.agent_terminals_unrestricted,
            dictation_mic: settings.dictation_mic,
            dictation_model: crate::speech::effective(&settings.dictation_model).key.clone(),
            push_to_talk_key: serde_json::to_value(settings.push_to_talk_key)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            claude_chat_ignores_api_key: settings.claude_chat_billing.ignores_api_key(),
            codex_subscription_balancing: settings.codex_subscription_balancing,
            dashboard_haiku_summaries: settings.dashboard_haiku_summaries,
            orchestrator_mode: serde_json::to_value(settings.orchestrator_mode)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            orchestrator_chat_provider: forms::provider_word(settings.orchestrator_chat_provider)
                .into(),
            orchestrator_chat_model: settings.orchestrator_chat_model.clone(),
            orchestrator_unreadable_note: settings.orchestrator_unreadable_note(),
            model_choices: forms::model_choices(
                settings.orchestrator_chat_provider,
                settings.orchestrator_chat_model.as_deref(),
            ),
            delegation: DelegationDto::global(settings),
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
    native_terminal_colors: Option<bool>,
    use_riwork_colors: Option<bool>,
    open_preview_on_select: Option<bool>,
    remember_window_size: Option<bool>,
    panel_tab_icons: Option<bool>,
    project_order: Option<riwork_core::project_sort::ProjectOrder>,
    tab_close_behavior: Option<riwork_core::settings::TabCloseBehavior>,
    agent_inline_mode: Option<bool>,
    agent_terminals_unrestricted: Option<bool>,
    dictation_mic: Option<bool>,
    dictation_model: Option<String>,
    push_to_talk_key: Option<riwork_core::settings::PushToTalkKey>,
    claude_chat_ignores_api_key: Option<bool>,
    codex_subscription_balancing: Option<bool>,
    dashboard_haiku_summaries: Option<bool>,
    orchestrator_mode: Option<riwork_core::settings::OrchestratorMode>,
    orchestrator_chat_provider: Option<riwork_core::provider::Provider>,
    orchestrator_chat_model: Option<Option<String>>,
    delegation_patch: Option<DelegationPatch>,
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
        if let Some(patch) = &self.delegation_patch {
            patch.validate()?;
        }
        let orchestrator_chat_model = self
            .orchestrator_chat_model
            .as_ref()
            .map(|value| value.as_deref().map(forms::checked_model).transpose())
            .transpose()?;
        Ok(ValidatedPatch {
            theme: self.theme.as_deref().map(parse_theme).transpose()?,
            native_terminal_colors: self.native_terminal_colors,
            use_riwork_colors: self.use_riwork_colors,
            open_preview_on_select: self.open_preview_on_select,
            remember_window_size: self.remember_window_size,
            panel_tab_icons: self.panel_tab_icons,
            project_order: self.project_order.as_ref().map(|order| {
                let by = serde_json::from_value(serde_json::json!(order.sort))
                    .map_err(|_| format!("invalid: unknown project sort {:?}", order.sort))?;
                Ok::<_, String>(riwork_core::project_sort::ProjectOrder { by, descending: order.descending })
            }).transpose()?,
            tab_close_behavior: self
                .tab_close_behavior
                .as_ref()
                .map(|value| {
                    serde_json::from_value(serde_json::json!(value))
                        .map_err(|_| format!("invalid: unknown tab_close_behavior {value:?}"))
                })
                .transpose()?,
            agent_inline_mode: self.agent_inline_mode,
            agent_terminals_unrestricted: self.agent_terminals_unrestricted,
            dictation_mic: self.dictation_mic,
            dictation_model: self
                .dictation_model
                .as_ref()
                .map(|key| {
                    crate::speech::model(key)
                        .map(|spec| spec.key.clone())
                        .ok_or_else(|| format!("invalid: unknown dictation_model {key:?}"))
                })
                .transpose()?,
            push_to_talk_key: self
                .push_to_talk_key
                .as_ref()
                .map(|value| {
                    serde_json::from_value(serde_json::json!(value))
                        .map_err(|_| format!("invalid: unknown push_to_talk_key {value:?}"))
                })
                .transpose()?,
            claude_chat_ignores_api_key: self.claude_chat_ignores_api_key,
            codex_subscription_balancing: self.codex_subscription_balancing,
            dashboard_haiku_summaries: self.dashboard_haiku_summaries,
            orchestrator_mode: self
                .orchestrator_mode
                .as_ref()
                .map(|value| {
                    serde_json::from_value(serde_json::json!(value))
                        .map_err(|_| format!("invalid: unknown orchestrator_mode {value:?}"))
                })
                .transpose()?,
            orchestrator_chat_provider: self
                .orchestrator_chat_provider
                .as_deref()
                .map(forms::parse_provider)
                .transpose()?,
            orchestrator_chat_model,
            delegation_patch: self.delegation_patch.clone(),
            ui_text_size: self.ui_text_size.map(TextPoints::new),
            ui_text_matches_terminal: self.ui_text_matches_terminal,
            interface_font,
            status_bar,
        })
    }
}

impl ValidatedPatch {
    fn apply(self, settings: &mut CoreSettings) {
        if let Some(value) = self.remember_window_size {
            settings.remember_window_size = value;
        }
        if let Some(value) = self.panel_tab_icons {
            settings.panel_tab_icons = value;
        }
        if let Some(value) = self.project_order {
            settings.project_order = value;
        }
        if let Some(value) = self.tab_close_behavior {
            settings.tab_close_behavior = value;
        }
        if let Some(value) = self.agent_inline_mode {
            settings.agent_inline_mode = value;
        }
        if let Some(value) = self.agent_terminals_unrestricted {
            settings.agent_terminals_unrestricted = value;
        }
        if let Some(value) = self.dictation_mic {
            settings.dictation_mic = value;
        }
        if let Some(value) = &self.dictation_model {
            settings.dictation_model = value.clone();
        }
        if let Some(value) = self.push_to_talk_key {
            settings.push_to_talk_key = value;
        }
        if let Some(value) = self.claude_chat_ignores_api_key {
            settings.claude_chat_billing =
                riwork_core::provider::ClaudeBilling::from_ignores_api_key(value);
        }
        if let Some(value) = self.codex_subscription_balancing {
            settings.codex_subscription_balancing = value;
        }
        if let Some(value) = self.dashboard_haiku_summaries {
            settings.dashboard_haiku_summaries = value;
        }
        if let Some(value) = self.orchestrator_mode {
            settings.choose_orchestrator_mode(value);
        }
        if let Some(value) = self.orchestrator_chat_provider {
            settings.choose_orchestrator_provider(value);
        }
        if let Some(value) = self.orchestrator_chat_model {
            settings.choose_orchestrator_model(value);
        }
        if let Some(patch) = self.delegation_patch {
            patch.apply(&mut settings.delegation_policy);
        }
        if let Some(value) = self.theme {
            settings.theme = value;
        }
        if let Some(value) = self.open_preview_on_select {
            settings.open_preview_on_select = value;
        }
        if let Some(value) = self.native_terminal_colors {
            settings.native_terminal_colors = value;
        }
        if let Some(value) = self.use_riwork_colors {
            settings.use_riwork_colors = value;
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
    UsageMode(riwork_core::status_bar::UsageMode),
    Reset,
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
            (patch.remember_window_size.is_some(), "remember_window_size"),
            (patch.panel_tab_icons.is_some(), "panel_tab_icons"),
            (patch.project_order.is_some(), "project_order"),
            (patch.tab_close_behavior.is_some(), "tab_close_behavior"),
            (patch.agent_inline_mode.is_some(), "agent_inline_mode"),
            (
                patch.agent_terminals_unrestricted.is_some(),
                "agent_terminals_unrestricted",
            ),
            (patch.dictation_mic.is_some(), "dictation_mic"),
            (patch.dictation_model.is_some(), "dictation_model"),
            (patch.push_to_talk_key.is_some(), "push_to_talk_key"),
            (
                patch.claude_chat_ignores_api_key.is_some(),
                "claude_chat_ignores_api_key",
            ),
            (
                patch.codex_subscription_balancing.is_some(),
                "codex_subscription_balancing",
            ),
            (
                patch.dashboard_haiku_summaries.is_some(),
                "dashboard_haiku_summaries",
            ),
            (patch.orchestrator_mode.is_some(), "orchestrator_mode"),
            (
                patch.orchestrator_chat_provider.is_some(),
                "orchestrator_chat_provider",
            ),
            (
                patch.orchestrator_chat_provider.is_some()
                    || patch.orchestrator_chat_model.is_some(),
                "orchestrator_chat_model",
            ),
            (patch.delegation_patch.is_some(), "delegation_policy"),
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
            (
                patch.open_preview_on_select.is_some(),
                "open_preview_on_select",
            ),
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
                StatusChange::UsageMode(mode) => settings.status_bar.usage_mode = mode,
                StatusChange::Reset => settings.status_bar = Default::default(),
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
    pub(crate) fn core(&self) -> (CoreSettings, Option<String>) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (state.settings.clone(), state.error.clone())
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
    pub async fn set_usage_mode(mode: String) -> Result<SettingsDto, String> {
        let mode = serde_json::from_value(serde_json::json!(mode))
            .map_err(|_| "invalid: unknown account usage mode".to_string())?;
        let preferences = service::initialize().await?.preferences.clone();
        blocking(move || preferences.change_status(StatusChange::UsageMode(mode))).await
    }
    pub async fn reset_status_bar() -> Result<SettingsDto, String> {
        let preferences = service::initialize().await?.preferences.clone();
        blocking(move || preferences.change_status(StatusChange::Reset)).await
    }
    pub fn sentence_case(text: String) -> String {
        ui_text::sentence_case(&text)
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

pub(crate) use snapshot_js_value;

snapshot_js_value!(SettingsDto, "SettingsEvent");

// JsObject intentionally decodes only declared properties. Patches instead use
// the complete JSON object so serde can reject unknown caller-supplied keys.
impl JsValue for SettingsPatch {
    fn ts_type(types: &mut TsTypes) -> String {
        types.declare("SettingsPatch", |types| format!(
            "export interface SettingsPatch {{ theme?: string; nativeTerminalColors?: boolean; useRiworkColors?: boolean; projectOrder?: {{ sort: 'name' | 'last_edited' | 'date_added' | 'live_sessions'; descending: boolean }}; openPreviewOnSelect?: boolean; uiTextSize?: number; uiTextMatchesTerminal?: boolean; interfaceFont?: 'system' | 'system_mono'; rememberWindowSize?: boolean; panelTabIcons?: boolean; tabCloseBehavior?: 'ask' | 'detach' | 'exit'; agentInlineMode?: boolean; agentTerminalsUnrestricted?: boolean; dictationMic?: boolean; dictationModel?: string; pushToTalkKey?: 'right_command' | 'right_option' | 'off'; claudeChatIgnoresApiKey?: boolean; codexSubscriptionBalancing?: boolean; dashboardHaikuSummaries?: boolean; orchestratorMode?: 'automatic' | 'terminal' | 'chat'; orchestratorChatProvider?: 'codex' | 'claude' | 'muse'; orchestratorChatModel?: string | null; statusBar?: {}; delegationPatch?: {}; }}",
            StatusBarDto::ts_type(types), DelegationPatch::ts_type(types)))
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
