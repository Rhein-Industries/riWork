//! Saved application preferences, shared by every RiWork window and process.
//! UI globals and panels adapt these data types in the desktop crate.

use crate::{
    delegation::DelegationPolicy,
    project_sort::ProjectOrder,
    provider::{ClaudeBilling, Provider},
    status_bar::StatusBarSettings,
    store::{FileStamp, ParseCache, parse_cache_for},
    theme::ThemeChoice,
    ui_text::{InterfaceFont, TextPoints},
};
use fs2::FileExt;
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    fs,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    #[default]
    Normal,
    Verbose,
}
impl DisplayMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Verbose => "Verbose",
        }
    }
}
/// How an orchestrator created from now on runs: chosen by RiWork (`Automatic`), in a
/// terminal tab, as it always has, or as a chat of the chat host.
///
/// `Automatic` is the default, so a settings file without the key (a fresh one, or
/// one from a build before orchestrators had modes) gets it. A saved `terminal` or
/// `chat` is a choice and stays one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorMode {
    #[default]
    Automatic,
    Terminal,
    Chat,
}

impl OrchestratorMode {
    pub const ALL: [Self; 3] = [Self::Automatic, Self::Terminal, Self::Chat];

    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::Terminal => "Terminal",
            Self::Chat => "Chat",
        }
    }
}

/// The chats `Automatic` starts an orchestrator as, in order: Muse on Wonder Full,
/// the best delegator, and Codex on GPT-6.1-Sol, the workhorse, when Muse cannot
/// start.
pub const AUTOMATIC_ORCHESTRATORS: [(Provider, &str); 2] = [
    (Provider::Muse, "muse-wonder-full-1.0"),
    (Provider::Codex, "gpt-6.1-sol"),
];

/// The model RiWork's routing prefers for each chat provider: Muse Wonder Full for
/// orchestration, Claude Opus 5.5 (the CLI's `opus` alias) for the hardest
/// reasoning and review, Codex GPT-6.1-Sol as the workhorse. Settings offer it
/// beside the provider's own default.
pub fn recommended_model(provider: Provider) -> &'static str {
    match provider {
        Provider::Muse => "muse-wonder-full-1.0",
        Provider::Claude => "opus",
        Provider::Codex => "gpt-6.1-sol",
    }
}

/// Whether `model` can be saved as a model id: what a chat accepts (not blank, at
/// most 100 characters, no control characters).
pub fn valid_model_id(model: &str) -> bool {
    !model.trim().is_empty() && model.chars().count() <= 100 && !model.chars().any(char::is_control)
}

/// What an orchestrator created now is: chosen by RiWork, a terminal, or a chat of
/// a provider on a model (`None` is the provider's own default). Settings name one
/// for every orchestrator, and a project's override for its own
/// (`store::ProjectOrchestrator`), in the same shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum OrchestratorRuns {
    /// A Muse chat on Wonder Full, else a Codex chat on GPT-6.1-Sol
    /// (`AUTOMATIC_ORCHESTRATORS`), as the first that starts.
    Automatic,
    /// A Codex terminal, as orchestrators always ran.
    Terminal,
    Chat {
        provider: Provider,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
}

impl<'de> Deserialize<'de> for OrchestratorRuns {
    /// Strict: a field this build does not know, a provider it lacks or a model id a
    /// chat would refuse makes the whole value unreadable, so a caller keeps it as
    /// it was saved rather than reading part of it.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
        enum Shape {
            Automatic {},
            Terminal {},
            Chat {
                provider: Provider,
                #[serde(default)]
                model: Option<String>,
            },
        }
        Ok(match Shape::deserialize(deserializer)? {
            Shape::Automatic {} => Self::Automatic,
            Shape::Terminal {} => Self::Terminal,
            Shape::Chat { provider, model } => {
                if let Some(model) = &model
                    && !valid_model_id(model)
                {
                    return Err(serde::de::Error::custom(format!(
                        "invalid model id {model:?}"
                    )));
                }
                Self::Chat { provider, model }
            }
        })
    }
}

impl OrchestratorRuns {
    /// A chat on the provider's own default model.
    pub fn chat(provider: Provider) -> Self {
        Self::Chat {
            provider,
            model: None,
        }
    }

    pub fn mode(&self) -> OrchestratorMode {
        match self {
            Self::Automatic => OrchestratorMode::Automatic,
            Self::Terminal => OrchestratorMode::Terminal,
            Self::Chat { .. } => OrchestratorMode::Chat,
        }
    }

    /// Whether an orchestrator made this way is a chat.
    pub fn is_chat(&self) -> bool {
        !matches!(self, Self::Terminal)
    }

    /// The choice in words, for Settings, notices and the CLI.
    pub fn describe(&self) -> String {
        let [(first, first_model), (second, second_model)] = AUTOMATIC_ORCHESTRATORS;
        match self {
            Self::Automatic => format!(
                "Automatic: a {} chat on {first_model}, or a {} chat on {second_model} when {} cannot start",
                provider_label(first),
                provider_label(second),
                provider_label(first),
            ),
            Self::Terminal => "a Codex terminal".to_owned(),
            Self::Chat {
                provider,
                model: Some(model),
            } => format!("a {} chat on {model}", provider_label(*provider)),
            Self::Chat {
                provider,
                model: None,
            } => format!("a {} chat on its default model", provider_label(*provider)),
        }
    }
}

/// A chat provider as people know it.
pub fn provider_label(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
        Provider::Muse => "Muse",
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabCloseBehavior {
    #[default]
    Ask,
    Detach,
    Exit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Settings {
    pub tab_close_behavior: TabCloseBehavior,
    pub chat_display: DisplayMode,
    /// Conversation overrides; chat_display remains the default for untouched chats.
    pub chat_display_modes: BTreeMap<String, DisplayMode>,
    pub schema_version: u32,
    /// Always Native. A removed theme (or a newer build's) in the file reads as
    /// Native and stays as written until the setting changes.
    pub theme: ThemeChoice,
    pub remember_window_size: bool,
    /// Built-in panel tabs and toolbar buttons show an icon instead of their text label.
    /// The key predates the toolbar buttons and stays as it is in `settings.json`.
    pub panel_tab_icons: bool,
    /// Selecting a file in Files opens the Preview panel, or brings it forward if it is
    /// hidden behind another tab. Off leaves the panes as the user arranged them, so a
    /// Preview tab that was closed stays closed.
    pub open_preview_on_select: bool,
    pub project_order: ProjectOrder,
    pub selected_codex_account: Option<String>,
    /// New Codex sessions that inherit the app's account choose the saved subscription
    /// with the most remaining quota. Explicit accounts, project pins and resumes keep
    /// their identity. Off until the person chooses it.
    pub codex_subscription_balancing: bool,
    /// Optional separately billed API summaries of observed dashboard activity.
    pub dashboard_haiku_summaries: bool,
    pub status_bar: StatusBarSettings,
    /// New agent sessions (Codex, Grok, Claude Code) run inline on the
    /// terminal's main screen instead of its alternate screen, so the
    /// conversation becomes tmux scrollback that a remote viewer can fetch and
    /// scroll locally. Off leaves every agent on its own default screen. A
    /// session that is already running keeps the mode it started in.
    pub agent_inline_mode: bool,
    /// Codex, Claude, Grok and Muse terminals opened from the New tab menu, their shortcuts and
    /// the paired phone start without approval prompts (Codex
    /// `--dangerously-bypass-approvals-and-sandbox`, Claude `--dangerously-skip-permissions`,
    /// Grok `--always-approve`, Muse `--yolo`). Off, they ask before they act. A session that is already
    /// running keeps the mode it started in.
    pub agent_terminals_unrestricted: bool,
    /// How orchestrators created from now on run. An orchestrator that exists
    /// keeps its mode until it is closed and created again.
    pub orchestrator_mode: OrchestratorMode,
    /// The agent behind a chat orchestrator (`orchestrator_mode` is `chat`).
    pub orchestrator_chat_provider: Provider,
    /// The model of a chat orchestrator (`orchestrator_mode` is `chat`); none is the
    /// provider's own default.
    pub orchestrator_chat_model: Option<String>,
    /// The orchestrator keys whose saved values this build cannot read, with those
    /// values, so Settings can say why they read as defaults. The file keeps them
    /// until the setting is changed. Never saved.
    #[serde(skip)]
    pub orchestrator_unreadable: Vec<(String, Value)>,
    /// How every orchestrator spreads work over worker shells and which models it
    /// prefers, where a project's own policy (`Project::delegation_policy`) chooses
    /// nothing. Told to an orchestrator when it starts or loads its skill; one that
    /// runs keeps what it was told. Written only once something is chosen in it.
    pub delegation_policy: DelegationPolicy,
    /// RiWork's own interface text in points of its panel list text, like
    /// Ghostty's `font-size`: 11 is the design size, from `ui_text::MIN_POINTS`
    /// to `MAX_POINTS`. Terminals keep Ghostty's size.
    pub ui_text_size: TextPoints,
    /// Interface text follows Ghostty's `font-size` instead of `ui_text_size`.
    pub ui_text_matches_terminal: bool,
    /// "Terminals match the theme": terminals take Native's white or black
    /// background and palette. Off keeps Ghostty's own colors. The only terminal
    /// colors option: `use_riwork_colors`, which older builds wrote for the
    /// removed Ghostty theme, is no longer read and stays in the file as written.
    pub native_terminal_colors: bool,
    /// The interface font.
    pub interface_font: InterfaceFont,
    /// Microphone buttons for dictation: in the Mac's chat message boxes, and on the
    /// paired phone (its chats and terminals), which reads it from `appearance.json` as
    /// `mic`. Off, the Mac's chats show no mic, ⌃⌥D does nothing in them, and no speech
    /// helper runs or asks for a permission.
    pub dictation_mic: bool,
    /// Who a new Claude chat bills (`ClaudeBilling`); each chat then keeps its own choice
    /// (`ChatInfo::claude_billing`). `Subscription` starts chats without
    /// `ANTHROPIC_API_KEY`, so a key exported in the shell cannot switch them to API
    /// billing. `ApiKey`, the default, gives them the key saved in the Keychain, or runs
    /// `claude` as published, with every sign-in method it has; Anthropic's terms for
    /// products that run Claude Code ask that none be removed.
    ///
    /// Saved as the switch it grew from, `claude_chat_ignores_api_key` (true is
    /// `Subscription`), so builds before the choice, and a chat host of one still
    /// running, read the same choice from the same key, and no second key can disagree.
    #[serde(
        rename = "claude_chat_ignores_api_key",
        serialize_with = "serialize_billing_as_flag"
    )]
    pub claude_chat_billing: ClaudeBilling,
}

fn serialize_billing_as_flag<S: serde::Serializer>(
    billing: &ClaudeBilling,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_bool(billing.ignores_api_key())
}

impl Settings {
    /// Known persisted keys and their typed values, using the settings file's names.
    /// Runtime-only diagnostics (such as `orchestrator_unreadable`) are excluded.
    pub fn values(&self) -> Result<Map<String, Value>, String> {
        match serde_json::to_value(self) {
            Ok(Value::Object(fields)) => Ok(fields),
            Ok(_) => Err("Cannot encode settings: not an object".to_owned()),
            Err(error) => Err(format!("Cannot encode settings: {error}")),
        }
    }

    /// What an orchestrator created now runs as by these settings.
    pub fn orchestrator_runs(&self) -> OrchestratorRuns {
        match self.orchestrator_mode {
            OrchestratorMode::Automatic => OrchestratorRuns::Automatic,
            OrchestratorMode::Terminal => OrchestratorRuns::Terminal,
            OrchestratorMode::Chat => OrchestratorRuns::Chat {
                provider: self.orchestrator_chat_provider,
                model: self.orchestrator_chat_model.clone(),
            },
        }
    }

    /// Makes `runs` the setting. A choice other than a chat keeps the saved chat
    /// provider and model, for when chat is chosen again.
    pub fn set_orchestrator_runs(&mut self, runs: &OrchestratorRuns) {
        self.choose_orchestrator_mode(runs.mode());
        if let OrchestratorRuns::Chat { provider, model } = runs {
            self.choose_orchestrator_provider(*provider);
            self.choose_orchestrator_model(model.clone());
        }
    }

    /// "Orchestrator runs as" chosen. Choosing is what replaces a saved value this build
    /// could not read, even with the default that stood in for it (`merged_document`).
    pub fn choose_orchestrator_mode(&mut self, mode: OrchestratorMode) {
        self.orchestrator_mode = mode;
        self.forget_unreadable("orchestrator_mode");
    }

    /// "Chat provider" chosen: another provider, or one chosen over an unreadable value,
    /// starts on its own default model, as a new chat does when its provider changes.
    pub fn choose_orchestrator_provider(&mut self, provider: Provider) {
        let unreadable = self.is_unreadable("orchestrator_chat_provider");
        if provider != self.orchestrator_chat_provider || unreadable {
            self.choose_orchestrator_model(None);
        }
        self.orchestrator_chat_provider = provider;
        self.forget_unreadable("orchestrator_chat_provider");
    }

    /// "Chat model" chosen (`None`: the provider's own default).
    pub fn choose_orchestrator_model(&mut self, model: Option<String>) {
        self.orchestrator_chat_model = model;
        self.forget_unreadable("orchestrator_chat_model");
    }

    fn is_unreadable(&self, key: &str) -> bool {
        self.orchestrator_unreadable
            .iter()
            .any(|(unreadable, _)| unreadable == key)
    }

    fn forget_unreadable(&mut self, key: &str) {
        self.orchestrator_unreadable
            .retain(|(unreadable, _)| unreadable != key);
    }

    /// What Settings should say about saved orchestrator values this build cannot
    /// read, if there are any.
    pub fn orchestrator_unreadable_note(&self) -> Option<String> {
        unreadable_note(&self.orchestrator_unreadable)
    }

    pub fn chat_display_for(&self, chat_id: Option<&str>) -> DisplayMode {
        chat_id
            .and_then(|id| self.chat_display_modes.get(id))
            .copied()
            .unwrap_or(self.chat_display)
    }
    /// Whether "Terminals match the theme" is on.
    pub fn terminal_colors_forced(&self) -> bool {
        self.native_terminal_colors
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            tab_close_behavior: TabCloseBehavior::Ask,
            chat_display: Default::default(),
            chat_display_modes: BTreeMap::new(),
            theme: ThemeChoice::Native,
            remember_window_size: true,
            panel_tab_icons: false,
            open_preview_on_select: true,
            project_order: ProjectOrder::default(),
            selected_codex_account: None,
            codex_subscription_balancing: false,
            dashboard_haiku_summaries: false,
            status_bar: StatusBarSettings::default(),
            agent_inline_mode: true,
            agent_terminals_unrestricted: true,
            orchestrator_mode: OrchestratorMode::default(),
            orchestrator_chat_provider: Provider::Codex,
            orchestrator_chat_model: None,
            orchestrator_unreadable: Vec::new(),
            delegation_policy: DelegationPolicy::default(),
            ui_text_size: TextPoints::DEFAULT,
            ui_text_matches_terminal: false,
            native_terminal_colors: true,
            interface_font: InterfaceFont::default(),
            dictation_mic: false,
            claude_chat_billing: ClaudeBilling::default(),
        }
    }
}

impl<'de> Deserialize<'de> for Settings {
    /// Every field falls back to its default on its own, so a value that a
    /// newer build wrote (a theme or sort order this build lacks) cannot make
    /// the whole file unreadable. The schema version and the Codex account
    /// selection stay strict: guessing there would launch agents on the wrong
    /// account or rewrite a format this build does not understand.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let object = Map::<String, Value>::deserialize(deserializer)?;
        let defaults = Self::default();
        Ok(Self {
            tab_close_behavior: lenient_field(
                &object,
                "tab_close_behavior",
                defaults.tab_close_behavior,
            ),
            chat_display: lenient_field(&object, "chat_display", defaults.chat_display),
            chat_display_modes: object
                .get("chat_display_modes")
                .and_then(Value::as_object)
                .map(|modes| {
                    modes
                        .iter()
                        .filter_map(|(id, mode)| {
                            serde_json::from_value(mode.clone())
                                .ok()
                                .map(|mode| (id.clone(), mode))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            schema_version: strict_field(&object, "schema_version", defaults.schema_version)?,
            theme: lenient_field(&object, "theme", defaults.theme),
            remember_window_size: lenient_field(
                &object,
                "remember_window_size",
                defaults.remember_window_size,
            ),
            panel_tab_icons: lenient_field(&object, "panel_tab_icons", defaults.panel_tab_icons),
            open_preview_on_select: lenient_field(
                &object,
                "open_preview_on_select",
                defaults.open_preview_on_select,
            ),
            project_order: lenient_field(&object, "project_order", defaults.project_order),
            selected_codex_account: strict_field(
                &object,
                "selected_codex_account",
                defaults.selected_codex_account,
            )?,
            codex_subscription_balancing: strict_field(
                &object,
                "codex_subscription_balancing",
                defaults.codex_subscription_balancing,
            )?,
            dashboard_haiku_summaries: strict_field(
                &object,
                "dashboard_haiku_summaries",
                defaults.dashboard_haiku_summaries,
            )?,
            status_bar: lenient_field(&object, "status_bar", defaults.status_bar),
            agent_inline_mode: lenient_field(
                &object,
                "agent_inline_mode",
                defaults.agent_inline_mode,
            ),
            agent_terminals_unrestricted: lenient_field(
                &object,
                "agent_terminals_unrestricted",
                defaults.agent_terminals_unrestricted,
            ),
            orchestrator_mode: lenient_field(
                &object,
                "orchestrator_mode",
                defaults.orchestrator_mode,
            ),
            orchestrator_chat_provider: lenient_field(
                &object,
                "orchestrator_chat_provider",
                defaults.orchestrator_chat_provider,
            ),
            // A model id means something only with its provider: one saved beside a
            // provider this build cannot read stands down too (and stays in the file).
            orchestrator_chat_model: lenient_field::<Option<String>>(
                &object,
                "orchestrator_chat_model",
                None,
            )
            .filter(|model| valid_model_id(model))
            .filter(|_| {
                object
                    .get("orchestrator_chat_provider")
                    .is_none_or(|provider| Provider::deserialize(provider).is_ok())
            }),
            orchestrator_unreadable: [
                "orchestrator_mode",
                "orchestrator_chat_provider",
                "orchestrator_chat_model",
            ]
            .into_iter()
            .filter_map(|key| {
                let value = object.get(key)?;
                let provider_readable = object
                    .get("orchestrator_chat_provider")
                    .is_none_or(|provider| Provider::deserialize(provider).is_ok());
                let readable = match key {
                    "orchestrator_mode" => OrchestratorMode::deserialize(value).is_ok(),
                    "orchestrator_chat_provider" => provider_readable,
                    // A model is unusable beside a provider that cannot be read.
                    _ => Option::<String>::deserialize(value).is_ok_and(|model| {
                        model.is_none_or(|model| valid_model_id(&model) && provider_readable)
                    }),
                };
                (!readable).then(|| (key.to_owned(), value.clone()))
            })
            .collect(),
            // Field by field: what this build cannot read is kept in the policy.
            delegation_policy: lenient_field(
                &object,
                "delegation_policy",
                defaults.delegation_policy,
            ),
            // A size out of range is pulled into it; anything but a number is the default.
            ui_text_size: lenient_field(&object, "ui_text_size", defaults.ui_text_size),
            ui_text_matches_terminal: lenient_field(
                &object,
                "ui_text_matches_terminal",
                defaults.ui_text_matches_terminal,
            ),
            native_terminal_colors: lenient_field(
                &object,
                "native_terminal_colors",
                defaults.native_terminal_colors,
            ),
            interface_font: lenient_field(&object, "interface_font", defaults.interface_font),
            dictation_mic: lenient_field(&object, "dictation_mic", defaults.dictation_mic),
            // The switch the choice grew from: true is Subscription; false, a file
            // without it and a value of another shape are the default, API key.
            claude_chat_billing: ClaudeBilling::from_ignores_api_key(lenient_field(
                &object,
                "claude_chat_ignores_api_key",
                defaults.claude_chat_billing.ignores_api_key(),
            )),
        })
    }
}

/// "settings.json has orchestrator_mode = "hologram", which this RiWork cannot read; …"
fn unreadable_note(unreadable: &[(String, Value)]) -> Option<String> {
    if unreadable.is_empty() {
        return None;
    }
    let values = unreadable
        .iter()
        .map(|(key, value)| format!("{key} = {value}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "settings.json has {values}, which this RiWork cannot read or use. It is kept as saved, and the default stands in for it until you choose here."
    ))
}

fn lenient_field<T: DeserializeOwned>(object: &Map<String, Value>, key: &str, default: T) -> T {
    object
        .get(key)
        .and_then(|value| T::deserialize(value).ok())
        .unwrap_or(default)
}

fn strict_field<T: DeserializeOwned, E: serde::de::Error>(
    object: &Map<String, Value>,
    key: &str,
    default: T,
) -> Result<T, E> {
    match object.get(key) {
        None => Ok(default),
        Some(value) => T::deserialize(value).map_err(|error| E::custom(format!("{key}: {error}"))),
    }
}

/// Whether an agent launched from the state directory `home` should run inline.
/// Launch code (including the `codex` and `grok` wrappers, which run as their
/// own processes) reads the file each time instead of a copy that a running
/// window holds. A missing or unreadable file means the default, so a damaged
/// settings file never decides how an agent draws.
pub fn agent_inline_mode(home: &Path) -> bool {
    SettingsStore::open(home)
        .and_then(|store| store.load())
        .map_or(Settings::default().agent_inline_mode, |settings| {
            settings.agent_inline_mode
        })
}

/// Whether a Codex, Claude, Grok or Muse terminal opened now from a menu, a shortcut or the
/// phone runs unrestricted, read from the file of the state directory `home` each time,
/// like `agent_inline_mode`, so every window and the CLI follow a change at once. A
/// missing or unreadable file means the default, unrestricted.
pub fn agent_terminals_unrestricted(home: &Path) -> bool {
    SettingsStore::open(home)
        .and_then(|store| store.load())
        .map_or(
            Settings::default().agent_terminals_unrestricted,
            |settings| settings.agent_terminals_unrestricted,
        )
}

/// Who a Claude chat of the state directory `home` bills when it has no choice of its
/// own (a new chat, or one made before chats had one), read from the file each time, like
/// `agent_inline_mode`: the chat host outlives the window that changed it. A missing or
/// unreadable file means the default, API key, which leaves the environment to `claude`
/// when no key is saved.
pub fn claude_chat_billing(home: &Path) -> ClaudeBilling {
    SettingsStore::open(home)
        .and_then(|store| store.load())
        .map_or(Settings::default().claude_chat_billing, |settings| {
            settings.claude_chat_billing
        })
}

/// What an orchestrator created now runs as by Settings, read from the file of the
/// state directory `home` each time, like `agent_inline_mode`: the CLI, the MCP
/// tools and every window create orchestrators, and none of them holds a copy that
/// another could outdate. A missing or unreadable file means the default,
/// `Automatic`. A project's own choice comes on top (`orchestrators::plan`).
pub fn orchestrator_runs(home: &Path) -> OrchestratorRuns {
    SettingsStore::open(home)
        .and_then(|store| store.load())
        .unwrap_or_default()
        .orchestrator_runs()
}

#[derive(Clone)]
pub struct SettingsStore {
    dir: PathBuf,
    /// The last `settings.json` parse and the stamp it was read under,
    /// shared by every settings store of this directory in this process.
    load_cache: ParseCache<Settings>,
}

impl SettingsStore {
    pub fn open_default() -> Result<Self, String> {
        Self::open(crate::paths::riwork_home()?)
    }

    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, String> {
        let dir = dir.into();
        if dir.as_os_str().is_empty() {
            return Err("The RiWork data directory path is empty".to_owned());
        }
        crate::paths::create_private_dir(&dir)
            .map_err(|error| format!("Cannot create settings directory: {error}"))?;
        static CACHES: Mutex<BTreeMap<PathBuf, ParseCache<Settings>>> = Mutex::new(BTreeMap::new());
        Ok(Self {
            load_cache: parse_cache_for(&CACHES, dir.join("settings.json")),
            dir,
        })
    }

    pub fn load(&self) -> Result<Settings, String> {
        let path = self.dir.join("settings.json");
        // Windows poll this every couple of seconds; while the file's stamp
        // stands, the last parse is the answer and no lock or read is needed.
        if let Some(stamp) = FileStamp::of(&path)
            && let Some(settings) = self.cached_load(&stamp)
        {
            return Ok(settings);
        }
        let lock = self.lock_file()?;
        FileExt::lock_shared(&lock).map_err(|error| format!("Cannot lock settings: {error}"))?;
        let before = FileStamp::of(&path);
        let settings = self.read()?;
        // Cache only when the file stood still across the read.
        if let Some(stamp) = before
            && FileStamp::of(&path) == Some(stamp)
        {
            *self
                .load_cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some((stamp, settings.clone()));
        }
        Ok(settings)
    }

    fn cached_load(&self, stamp: &Option<FileStamp>) -> Option<Settings> {
        self.load_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|(cached, _)| cached == stamp)
            .map(|(_, settings)| settings.clone())
    }

    pub fn update(&self, change: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
        self.update_with(&[], |before, _| {
            let mut settings = before.clone();
            change(&mut settings);
            Ok(settings)
        })
    }

    /// Apply a typed set of explicit choices under one write lock. Unlike an
    /// ordinary update, choosing a default replaces an unreadable future value
    /// that currently reads as that default. Other saved keys remain verbatim.
    pub fn update_selected(
        &self,
        chosen_keys: &[&str],
        change: impl FnOnce(&mut Settings),
    ) -> Result<Settings, String> {
        self.update_with(chosen_keys, |before, _| {
            let mut settings = before.clone();
            change(&mut settings);
            Ok(settings)
        })
    }

    /// Explicitly choose one known top-level setting. Validate against the latest
    /// complete document under the write lock; a refusal never reaches the save path.
    /// Input errors carry `invalid_request:`, as the CLI's structured errors do.
    pub fn set_value(&self, key: &str, value: Value) -> Result<Settings, String> {
        if !Settings::default().values()?.contains_key(key) {
            return Err(format!(
                "invalid_request: '{key}' is not a known setting; run riwork settings list"
            ));
        }
        self.update_with(&[key], |_, document| {
            let invalid = |reason: String| format!("invalid_request: Cannot set '{key}': {reason}");
            let mut prospective = document.clone();
            prospective.insert(key.to_owned(), value.clone());
            let settings = Settings::deserialize(Value::Object(prospective))
                .map_err(|error| invalid(error.to_string()))?;
            if settings.schema_version != 1 {
                return Err(invalid("only settings schema 1 is supported".to_owned()));
            }
            let effective = settings.values()?;
            validate_setting_input(key, &value, &effective[key], &settings).map_err(invalid)?;
            Ok(settings)
        })
    }

    fn update_with(
        &self,
        chosen_keys: &[&str],
        change: impl FnOnce(&Settings, &Map<String, Value>) -> Result<Settings, String>,
    ) -> Result<Settings, String> {
        let lock = self.lock_file()?;
        FileExt::lock_exclusive(&lock).map_err(|error| format!("Cannot lock settings: {error}"))?;
        let (before, document) = self.read_with_document()?;
        let settings = change(&before, &document)?;
        let document = merged_document(document, &before, &settings, chosen_keys)?;
        // What the file says now, including what it keeps that this build cannot
        // read (`orchestrator_unreadable`), is what was saved.
        let settings = Settings::deserialize(Value::Object(document.clone()))
            .map_err(|error| format!("Cannot encode settings: {error}"))?;
        let path = self.dir.join("settings.json");
        let temporary = self.dir.join(format!(".settings-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| format!("Cannot create settings: {error}"))?;
            serde_json::to_writer_pretty(&mut file, &document)
                .map_err(|error| format!("Cannot encode settings: {error}"))?;
            file.write_all(b"\n")
                .and_then(|_| file.sync_all())
                .map_err(|error| format!("Cannot save settings: {error}"))?;
            fs::rename(&temporary, &path)
                .map_err(|error| format!("Cannot replace settings: {error}"))?;
            File::open(&self.dir)
                .and_then(|dir| dir.sync_all())
                .map_err(|error| format!("Cannot sync settings directory: {error}"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result?;
        // The document just written parses to these settings, so seed the load
        // cache.
        *self
            .load_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner) =
            FileStamp::of(&path).map(|stamp| (stamp, settings.clone()));
        Ok(settings)
    }

    fn lock_file(&self) -> Result<File, String> {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join("settings.lock"))
            .map_err(|error| format!("Cannot open settings lock: {error}"))
    }

    fn read(&self) -> Result<Settings, String> {
        self.read_with_document().map(|(settings, _)| settings)
    }

    /// Also return the file's own JSON object: settings from another RiWork
    /// build may hold keys and values this one does not model.
    fn read_with_document(&self) -> Result<(Settings, Map<String, Value>), String> {
        let path = self.dir.join("settings.json");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Settings::default(), Map::new()));
            }
            Err(error) => return Err(format!("Cannot read settings: {error}")),
        };
        let document: Map<String, Value> = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Cannot parse settings: {error}"))?;
        let settings = Settings::deserialize(Value::Object(document.clone()))
            .map_err(|error| format!("Cannot parse settings: {error}"))?;
        if settings.schema_version != 1 {
            return Err(format!(
                "Unsupported settings schema {}",
                settings.schema_version
            ));
        }
        Ok((settings, document))
    }
}

/// Apply only what `change` altered to the file's own JSON. Unknown keys and
/// values this build read as defaults (a theme a newer build added) stay as
/// written until that setting changes to something other than what it read as.
///
/// The orchestrator keys (`CHOSEN_ONLY`) are written only when they are chosen: a value
/// that changed, or one chosen over a saved value this build could not read (it left
/// `orchestrator_unreadable`). So a file without them keeps meaning "never chosen", and
/// the default that stands for that can improve in a later build. The delegation policy
/// keeps what it cannot read itself, so writing it whole loses nothing.
/// `chosen_key` also saves an explicit CLI choice equal to the effective default:
/// choosing Native must replace a theme this build reads as Native.
fn merged_document(
    mut document: Map<String, Value>,
    before: &Settings,
    after: &Settings,
    chosen_keys: &[&str],
) -> Result<Map<String, Value>, String> {
    const CHOSEN_ONLY: [&str; 4] = [
        "orchestrator_mode",
        "orchestrator_chat_provider",
        "orchestrator_chat_model",
        "delegation_policy",
    ];
    let chosen_over_unreadable = |key: &str| before.is_unreadable(key) && !after.is_unreadable(key);
    let before_fields = before.values()?;
    let before = &before_fields;
    for (key, value) in after.values()? {
        let unchanged = before.get(&key) == Some(&value);
        if chosen_over_unreadable(&key) {
            document.insert(key, value);
            continue;
        }
        if unchanged
            && !chosen_keys.contains(&key.as_str())
            && (document.contains_key(&key) || CHOSEN_ONLY.contains(&key.as_str()))
        {
            continue;
        }
        if key == "chat_display_modes"
            && let Some(after_modes) = value.as_object()
            && let Some(before_modes) = before.get(&key).and_then(Value::as_object)
        {
            // Patch only entries the user changed. Other builds may have saved
            // per-chat values this build cannot decode into DisplayMode.
            let mut modes = document
                .get(&key)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for id in before_modes.keys() {
                if !after_modes.contains_key(id) {
                    modes.remove(id);
                }
            }
            for (id, mode) in after_modes {
                if before_modes.get(id) != Some(mode) {
                    modes.insert(id.clone(), mode.clone());
                }
            }
            document.insert(key, Value::Object(modes));
            continue;
        }
        document.insert(key, value);
    }
    Ok(document)
}

/// Settings intentionally loads many invalid fields as defaults. Merely succeeding
/// at deserialization is therefore insufficient for an explicit CLI write. Check
/// every supplied component against what the model actually understood, allowing
/// omitted object defaults and the status bar's appended default items.
fn validate_setting_input(
    key: &str,
    input: &Value,
    effective: &Value,
    settings: &Settings,
) -> Result<(), String> {
    if key == "delegation_policy"
        && (!settings.delegation_policy.unreadable.is_empty()
            || settings.delegation_policy.unreadable_whole.is_some())
    {
        return Err("the delegation policy contains unknown or unreadable values".to_owned());
    }
    if key == "ui_text_size" {
        // This field deliberately rounds to a tenth and clamps to 9..=24.
        // A non-number must still fail rather than silently become the default.
        return TextPoints::deserialize(input)
            .map(|_| ())
            .map_err(|error| error.to_string());
    }
    if key == "status_bar"
        && let Some(items) = input.get("items").and_then(Value::as_array)
    {
        for item in items {
            crate::status_bar::StatusBarItem::deserialize(item)
                .map_err(|error| format!("invalid status bar item: {error}"))?;
        }
    }
    retained_input(input, effective, key)
}

fn retained_input(input: &Value, effective: &Value, path: &str) -> Result<(), String> {
    match (input, effective) {
        (Value::Object(input), Value::Object(effective)) => {
            for (key, value) in input {
                let path = format!("{path}.{key}");
                let decoded = effective
                    .get(key)
                    .ok_or_else(|| format!("{path} is unknown or would be discarded"))?;
                retained_input(value, decoded, &path)?;
            }
            Ok(())
        }
        (Value::Array(input), Value::Array(effective))
            if path == "status_bar.items" && input.len() <= effective.len() =>
        {
            for (index, (value, decoded)) in input.iter().zip(effective).enumerate() {
                retained_input(value, decoded, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        _ if input == effective => Ok(()),
        _ => Err(format!(
            "{path} has the wrong type or an unsupported value (the app would read {effective})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_text::SizeChange;
    use std::env;

    #[test]
    fn optional_dashboard_api_and_subscription_balancing_are_strict_opt_in_settings() {
        let defaults = Settings::default();
        assert!(!defaults.dashboard_haiku_summaries);
        assert!(!defaults.codex_subscription_balancing);
        let dir = env::temp_dir().join(format!("riwork-settings-opt-in-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        for key in ["dashboard_haiku_summaries", "codex_subscription_balancing"] {
            assert_eq!(store.load().unwrap().values().unwrap()[key], false);
            store.set_value(key, Value::Bool(true)).unwrap();
            assert_eq!(store.load().unwrap().values().unwrap()[key], true);
            let before = fs::read(dir.join("settings.json")).unwrap();
            for bad in [Value::Null, serde_json::json!("yes"), serde_json::json!(1)] {
                assert!(store.set_value(key, bad).is_err());
                assert_eq!(fs::read(dir.join("settings.json")).unwrap(), before);
            }
            store.set_value(key, Value::Bool(false)).unwrap();
            assert_eq!(store.load().unwrap().values().unwrap()[key], false);
        }
        for key in ["dashboard_haiku_summaries", "codex_subscription_balancing"] {
            fs::write(dir.join("settings.json"), format!("{{\"{key}\":\"yes\"}}")).unwrap();
            assert!(
                store.load().is_err(),
                "a malformed opt-in cannot turn itself on or off"
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    /// Pure toggle fixtures for the configuration tests; controls live in desktop.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Toggle {
        TerminalColors,
        PanelTabIcons,
        PreviewOnSelect,
        AgentInline,
        AgentsUnrestricted,
        DictationMic,
        WindowSize,
    }

    impl Toggle {
        fn id(self) -> &'static str {
            match self {
                Self::TerminalColors => "terminal-colors",
                Self::PanelTabIcons => "panel-tab-icons",
                Self::PreviewOnSelect => "open-preview-on-select",
                Self::AgentInline => "agent-inline-mode",
                Self::AgentsUnrestricted => "agent-terminals-unrestricted",
                Self::DictationMic => "dictation-mic",
                Self::WindowSize => "remember-window-size",
            }
        }

        fn flip(self, settings: &mut Settings) {
            let value = match self {
                Self::TerminalColors => &mut settings.native_terminal_colors,
                Self::PanelTabIcons => &mut settings.panel_tab_icons,
                Self::PreviewOnSelect => &mut settings.open_preview_on_select,
                Self::AgentInline => &mut settings.agent_inline_mode,
                Self::AgentsUnrestricted => &mut settings.agent_terminals_unrestricted,
                Self::DictationMic => &mut settings.dictation_mic,
                Self::WindowSize => &mut settings.remember_window_size,
            };
            *value = !*value;
        }
    }

    /// The Text size row's MATCH TERMINAL control: interface text follows Ghostty's
    /// `font-size`, or goes back to the saved size.
    fn toggle_text_match(settings: &mut Settings) {
        settings.ui_text_matches_terminal = !settings.ui_text_matches_terminal;
    }

    #[test]
    fn saving_chat_modes_preserves_unknown_entries_until_that_chat_changes() {
        use crate::settings::DisplayMode::{Normal, Verbose};
        let dir = env::temp_dir().join(format!("riwork-future-chat-modes-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let initial = serde_json::json!({
            "schema_version": 1,
            "chat_display": "verbose",
            "chat_display_modes": {
                "known": "normal", "unchanged": "verbose", "future": "summary",
                "structured": {"mode": "future", "detail": 3}
            },
            "theme": "future-theme",
            "future_setting": {"keep": true},
            "open_preview_on_select": false
        });
        let path = dir.join("settings.json");
        fs::write(&path, serde_json::to_vec(&initial).unwrap()).unwrap();
        store
            .update(|settings| settings.dictation_mic = true)
            .unwrap();
        let read = || serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(read()["chat_display_modes"], initial["chat_display_modes"]);

        let reopened = SettingsStore::open(&dir).unwrap();
        reopened
            .update(|settings| {
                settings.chat_display_modes.insert("known".into(), Verbose);
                settings.chat_display_modes.insert("new".into(), Normal);
            })
            .unwrap();
        let saved = read();
        assert_eq!(saved["chat_display_modes"]["known"], "verbose");
        assert_eq!(saved["chat_display_modes"]["new"], "normal");
        for id in ["unchanged", "future", "structured"] {
            assert_eq!(
                saved["chat_display_modes"][id],
                initial["chat_display_modes"][id]
            );
        }
        for key in [
            "theme",
            "future_setting",
            "open_preview_on_select",
            "chat_display",
        ] {
            assert_eq!(saved[key], initial[key]);
        }
        assert_eq!(saved["dictation_mic"], true);
        let loaded = SettingsStore::open(&dir).unwrap().load().unwrap();
        assert_eq!(loaded.chat_display_for(Some("known")), Verbose);
        assert_eq!(loaded.chat_display_for(Some("new")), Normal);
        assert_eq!(loaded.chat_display_for(Some("future")), Verbose);

        // Explicitly selecting a supported mode for the unknown chat replaces
        // that entry alone; removing a known entry likewise leaves future data.
        reopened
            .update(|settings| {
                settings.chat_display_modes.insert("future".into(), Normal);
                settings.chat_display_modes.remove("known");
            })
            .unwrap();
        let saved = read();
        assert_eq!(saved["chat_display_modes"]["future"], "normal");
        assert!(saved["chat_display_modes"].get("known").is_none());
        assert_eq!(
            saved["chat_display_modes"]["structured"],
            initial["chat_display_modes"]["structured"]
        );
        assert_eq!(
            SettingsStore::open(&dir)
                .unwrap()
                .load()
                .unwrap()
                .chat_display_for(Some("future")),
            Normal
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn newer_values_and_keys_do_not_lock_out_settings_and_stay_until_changed() {
        let dir = env::temp_dir().join(format!("riwork-settings-newer-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"theme":"future_theme","project_order":{"by":"future_sort","descending":false},"remember_window_size":"yes","selected_codex_account":"saved-fixture-account","future_setting":{"a":1},"status_bar":{"enabled":false,"items":[{"kind":"future_widget"}]}}"#,
        )
        .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.theme, ThemeChoice::Native);
        assert_eq!(loaded.project_order, ProjectOrder::default());
        assert!(loaded.remember_window_size);
        assert_eq!(
            loaded.selected_codex_account.as_deref(),
            Some("saved-fixture-account")
        );
        assert!(!loaded.status_bar.enabled);

        // Changing one setting rewrites the file without touching the values
        // and keys this build could not read.
        let saved = store
            .update(|settings| settings.panel_tab_icons = true)
            .unwrap();
        assert!(saved.panel_tab_icons);
        let document = |dir: &std::path::Path| -> Value {
            serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap()
        };
        let file = document(&dir);
        assert_eq!(file["theme"], "future_theme");
        assert_eq!(file["project_order"]["by"], "future_sort");
        assert_eq!(file["remember_window_size"], "yes");
        assert_eq!(file["future_setting"], serde_json::json!({"a": 1}));
        assert_eq!(file["status_bar"]["items"][0]["kind"], "future_widget");
        assert_eq!(file["selected_codex_account"], "saved-fixture-account");
        assert_eq!(file["panel_tab_icons"], true);

        // A deliberate change replaces exactly that setting, even one that
        // chooses what an unreadable value already reads as.
        store
            .update_selected(&["theme"], |settings| {
                settings.theme = ThemeChoice::Native;
                settings.remember_window_size = false;
            })
            .unwrap();
        let file = document(&dir);
        assert_eq!(file["theme"], "native");
        assert_eq!(file["remember_window_size"], false);
        assert_eq!(file["project_order"]["by"], "future_sort");
        assert_eq!(file["future_setting"], serde_json::json!({"a": 1}));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_codex_selection_and_schema_fail_closed_while_other_damage_is_ignored() {
        let dir = env::temp_dir().join(format!("riwork-settings-strict-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let write = |content: &str| fs::write(dir.join("settings.json"), content).unwrap();
        write(r#"{"theme":7,"use_riwork_colors":"x","project_order":[],"status_bar":3}"#);
        assert_eq!(store.load().unwrap(), Settings::default());
        for content in [
            r#"{"selected_codex_account":5,"theme":"tokyo_night"}"#,
            r#"{"selected_codex_account":["a"]}"#,
            r#"{"schema_version":"one"}"#,
            r#"{"schema_version":2}"#,
            "[]",
        ] {
            write(content);
            assert!(store.load().is_err(), "{content}");
            assert!(store.update(|_| {}).is_err(), "{content}");
            assert_eq!(
                fs::read_to_string(dir.join("settings.json")).unwrap(),
                content
            );
        }
        write(r#"{"selected_codex_account":null}"#);
        assert_eq!(store.load().unwrap().selected_codex_account, None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_options_default_on_and_the_terminal_row_flips_native_terminal_colors() {
        let dir = env::temp_dir().join(format!("riwork-settings-native-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let defaults = Settings::default();
        assert!(defaults.native_terminal_colors);
        assert_eq!(defaults.interface_font, InterfaceFont::SystemMono);
        // A file from a build without them reads as the defaults; the retired
        // RiWork terminal colors key is not read.
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"theme":"ghostty","use_riwork_colors":true}"#,
        )
        .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.theme, ThemeChoice::Native);
        assert!(loaded.native_terminal_colors);
        assert!(loaded.terminal_colors_forced());
        assert_eq!(loaded.interface_font, InterfaceFont::SystemMono);

        let saved = store
            .update(|settings| Toggle::TerminalColors.flip(settings))
            .unwrap();
        assert!(!saved.native_terminal_colors);
        assert!(!saved.terminal_colors_forced());
        let saved = store
            .update(|settings| settings.interface_font = settings.interface_font.other())
            .unwrap();
        assert_eq!(saved.interface_font, InterfaceFont::System);
        let file: Value =
            serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(file["native_terminal_colors"], false);
        assert_eq!(file["interface_font"], "system");
        assert_eq!(file["theme"], "ghostty");
        assert_eq!(file["use_riwork_colors"], true);
        assert_eq!(store.load().unwrap(), saved);

        // A font this build does not know falls back to the default alone.
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"interface_font":"comic","native_terminal_colors":false}"#,
        )
        .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.interface_font, InterfaceFont::SystemMono);
        assert!(!loaded.native_terminal_colors);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_dictation_mic_is_off_by_default_and_persists_only_its_own_key() {
        let dir = env::temp_dir().join(format!("riwork-settings-mic-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let document = |dir: &std::path::Path| -> Value {
            serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap()
        };
        assert!(!Settings::default().dictation_mic);

        // A file from a build without the setting reads as off and is not rewritten.
        let older = r#"{"schema_version":1,"theme":"native","future_setting":[1]}"#;
        fs::write(dir.join("settings.json"), older).unwrap();
        assert!(!store.load().unwrap().dictation_mic);
        assert_eq!(
            fs::read_to_string(dir.join("settings.json")).unwrap(),
            older
        );

        // Turning it on writes it, keeps the rest, and a later change keeps it on.
        let saved = store
            .update(|settings| Toggle::DictationMic.flip(settings))
            .unwrap();
        assert!(saved.dictation_mic);
        let file = document(&dir);
        assert_eq!(file["dictation_mic"], true);
        assert_eq!(file["theme"], "native");
        assert_eq!(file["future_setting"], serde_json::json!([1]));
        store
            .update(|settings| settings.panel_tab_icons = true)
            .unwrap();
        assert!(store.load().unwrap().dictation_mic);
        store
            .update(|settings| Toggle::DictationMic.flip(settings))
            .unwrap();
        assert_eq!(document(&dir)["dictation_mic"], false);

        // A value of another shape reads as off.
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"dictation_mic":"yes"}"#,
        )
        .unwrap();
        assert!(!store.load().unwrap().dictation_mic);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_claude_billing_default_is_the_switch_it_grew_from_in_every_form_a_file_has() {
        use ClaudeBilling::{ApiKey, Subscription};
        let dir = env::temp_dir().join(format!("riwork-settings-api-key-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let path = dir.join("settings.json");
        let document = || serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(Settings::default().claude_chat_billing, ApiKey);
        // No file, a file from a build without the switch, the switch off, a value of
        // another shape: API key, which with no saved key leaves the environment to
        // Claude, as these builds and files always did. The switch on is Subscription.
        assert_eq!(claude_chat_billing(&dir), ApiKey);
        for (saved, billing) in [
            (r#"{"schema_version":1,"theme":"native"}"#, ApiKey),
            (
                r#"{"schema_version":1,"claude_chat_ignores_api_key":false}"#,
                ApiKey,
            ),
            (
                r#"{"schema_version":1,"claude_chat_ignores_api_key":true}"#,
                Subscription,
            ),
            (
                r#"{"schema_version":1,"claude_chat_ignores_api_key":"yes"}"#,
                ApiKey,
            ),
            (
                r#"{"schema_version":1,"claude_chat_ignores_api_key":null}"#,
                ApiKey,
            ),
            (
                r#"{"schema_version":1,"claude_chat_ignores_api_key":1}"#,
                ApiKey,
            ),
        ] {
            fs::write(&path, saved).unwrap();
            assert_eq!(claude_chat_billing(&dir), billing, "{saved}");
            assert_eq!(
                store.load().unwrap().claude_chat_billing,
                billing,
                "{saved}"
            );
        }
        // An unreadable file reads as the default.
        fs::write(&path, "{not json").unwrap();
        assert_eq!(claude_chat_billing(&dir), ApiKey);

        // A value of another shape stays as saved until the choice is made, and another
        // setting changing does not write the choice.
        fs::write(
            &path,
            r#"{"schema_version":1,"claude_chat_ignores_api_key":"yes"}"#,
        )
        .unwrap();
        store
            .update(|settings| settings.dictation_mic = true)
            .unwrap();
        assert_eq!(document()["claude_chat_ignores_api_key"], "yes");
        // Choosing writes the one key, as the switch every build reads, and no other: a
        // build from before the choice, or its chat host still running, reads the same.
        store
            .update(|settings| settings.claude_chat_billing = Subscription)
            .unwrap();
        assert_eq!(document()["claude_chat_ignores_api_key"], true);
        assert_eq!(claude_chat_billing(&dir), Subscription);
        store
            .update(|settings| settings.claude_chat_billing = ApiKey)
            .unwrap();
        assert_eq!(document()["claude_chat_ignores_api_key"], false);
        assert_eq!(claude_chat_billing(&dir), ApiKey);
        let keys = document()
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| key.contains("claude") || key.contains("billing"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(keys, ["claude_chat_ignores_api_key"]);

        // The choice changes only its own key.
        let base = serde_json::to_value(Settings::default()).unwrap();
        let chosen = serde_json::to_value(Settings {
            claude_chat_billing: Subscription,
            ..Settings::default()
        })
        .unwrap();
        let changed: Vec<_> = base
            .as_object()
            .unwrap()
            .iter()
            .filter(|(key, value)| chosen[key.as_str()] != **value)
            .map(|(key, _)| key.as_str())
            .collect();
        assert_eq!(changed, ["claude_chat_ignores_api_key"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn agent_terminals_unrestricted_defaults_on_and_survives_older_odd_and_unreadable_files() {
        let dir = env::temp_dir().join(format!("riwork-settings-unrestricted-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let document = |dir: &std::path::Path| -> Value {
            serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap()
        };
        assert!(Settings::default().agent_terminals_unrestricted);
        // No file yet: agents start unrestricted, and nothing is written.
        assert!(agent_terminals_unrestricted(&dir));
        assert!(!dir.join("settings.json").exists());

        // A file from a build without the setting reads as on and is not rewritten.
        let older = r#"{"schema_version":1,"agent_inline_mode":false,"future_setting":[2]}"#;
        fs::write(dir.join("settings.json"), older).unwrap();
        assert!(store.load().unwrap().agent_terminals_unrestricted);
        assert!(agent_terminals_unrestricted(&dir));
        assert_eq!(
            fs::read_to_string(dir.join("settings.json")).unwrap(),
            older
        );

        // Turning it off writes only that key, and the next launch sees it.
        let saved = store
            .update(|settings| Toggle::AgentsUnrestricted.flip(settings))
            .unwrap();
        assert!(!saved.agent_terminals_unrestricted);
        let file = document(&dir);
        assert_eq!(file["agent_terminals_unrestricted"], false);
        assert_eq!(file["agent_inline_mode"], false);
        assert_eq!(file["future_setting"], serde_json::json!([2]));
        assert!(!agent_terminals_unrestricted(&dir));
        store
            .update(|settings| Toggle::AgentsUnrestricted.flip(settings))
            .unwrap();
        assert!(agent_terminals_unrestricted(&dir));

        // A value of another shape keeps the default, and so does an unreadable file.
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"agent_terminals_unrestricted":"no"}"#,
        )
        .unwrap();
        assert!(store.load().unwrap().agent_terminals_unrestricted);
        assert!(agent_terminals_unrestricted(&dir));
        fs::write(dir.join("settings.json"), "{not json").unwrap();
        assert!(agent_terminals_unrestricted(&dir));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_delegation_policy_is_written_only_when_chosen_and_keeps_what_it_cannot_read() {
        use crate::delegation::{Delegation, Route, Work, resolve};
        let dir = env::temp_dir().join(format!("riwork-settings-delegation-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let document = || -> Value {
            serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap()
        };
        let write = |text: &str| fs::write(dir.join("settings.json"), text).unwrap();

        // A file from before the policy reads as nothing chosen, and saving something
        // else does not write it.
        write(r#"{"schema_version":1,"theme":"future_theme","future_setting":{"a":1}}"#);
        let loaded = store.load().unwrap();
        assert!(loaded.delegation_policy.is_empty());
        assert_eq!(
            resolve(&loaded.delegation_policy, None),
            Delegation::built_in()
        );
        store
            .update(|settings| settings.dictation_mic = true)
            .unwrap();
        assert!(document().get("delegation_policy").is_none());

        // Choosing writes just what is chosen, and leaves unrelated keys alone.
        store
            .update(|settings| {
                settings.delegation_policy.set_parallel_shells(Some(6));
                settings
                    .delegation_policy
                    .set_route(Work::Design, Some(Route::recommended(Provider::Muse)))
                    .unwrap();
            })
            .unwrap();
        let file = document();
        assert_eq!(
            file["delegation_policy"],
            serde_json::json!({
                "parallel_shells": 6,
                "design": {"provider": "muse", "model": "muse-wonder-full-1.0"}
            })
        );
        assert_eq!(file["theme"], "future_theme");
        assert_eq!(file["future_setting"], serde_json::json!({"a": 1}));
        assert_eq!(file["dictation_mic"], true);
        let policy = resolve(&store.load().unwrap().delegation_policy, None);
        assert_eq!(policy.parallel_shells, 6);
        assert!(policy.spread_work);
        assert_eq!(policy.design, Route::recommended(Provider::Muse));

        // Values this build cannot read stand down to the built-in policy, survive an
        // unrelated save and a change to another field of the policy, and go only when
        // their own field is chosen.
        write(
            r#"{"schema_version":1,"delegation_policy":{"spread_work":"often","parallel_shells":6,"workhorse":{"provider":"gemini","model":"x"},"future_field":true},"future_setting":2}"#,
        );
        let loaded = store.load().unwrap();
        assert_eq!(loaded.delegation_policy.parallel_shells, Some(6));
        assert_eq!(loaded.delegation_policy.unreadable.len(), 3);
        let resolved = resolve(&loaded.delegation_policy, None);
        assert!(resolved.spread_work);
        assert_eq!(resolved.workhorse, Work::Workhorse.built_in());
        store
            .update(|settings| settings.panel_tab_icons = true)
            .unwrap();
        store
            .update(|settings| settings.delegation_policy.set_parallel_shells(Some(2)))
            .unwrap();
        let file = document();
        assert_eq!(file["delegation_policy"]["parallel_shells"], 2);
        assert_eq!(file["delegation_policy"]["spread_work"], "often");
        assert_eq!(
            file["delegation_policy"]["workhorse"],
            serde_json::json!({"provider": "gemini", "model": "x"})
        );
        assert_eq!(file["delegation_policy"]["future_field"], true);
        assert_eq!(file["future_setting"], 2);
        store
            .update(|settings| settings.delegation_policy.set_spread_work(Some(false)))
            .unwrap();
        assert_eq!(document()["delegation_policy"]["spread_work"], false);

        // A policy that is not an object reads as nothing chosen and stays as saved
        // until the policy is changed.
        write(r#"{"schema_version":1,"delegation_policy":"spread"}"#);
        assert_eq!(
            resolve(&store.load().unwrap().delegation_policy, None),
            Delegation::built_in()
        );
        store
            .update(|settings| settings.dictation_mic = true)
            .unwrap();
        assert_eq!(document()["delegation_policy"], "spread");
        store
            .update(|settings| settings.delegation_policy.set_spread_work(Some(true)))
            .unwrap();
        assert_eq!(
            document()["delegation_policy"],
            serde_json::json!({"spread_work": true})
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn orchestrators_are_automatic_by_default_and_saved_choices_and_odd_values_survive() {
        let dir = env::temp_dir().join(format!("riwork-settings-orchestrator-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let document = |dir: &std::path::Path| -> Value {
            serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap()
        };
        let write = |text: &str| fs::write(dir.join("settings.json"), text).unwrap();
        let defaults = Settings::default();
        assert_eq!(defaults.orchestrator_mode, OrchestratorMode::Automatic);
        assert_eq!(defaults.orchestrator_chat_provider, Provider::Codex);
        assert_eq!(defaults.orchestrator_chat_model, None);
        assert_eq!(defaults.orchestrator_runs(), OrchestratorRuns::Automatic);
        // No file yet: automatic, and nothing is written.
        assert_eq!(orchestrator_runs(&dir), OrchestratorRuns::Automatic);
        assert!(!dir.join("settings.json").exists());

        // A file from a build without the settings reads as the default and is
        // not rewritten.
        let older = r#"{"schema_version":1,"theme":"tokyo_night","future_setting":{"a":1}}"#;
        write(older);
        assert_eq!(orchestrator_runs(&dir), OrchestratorRuns::Automatic);
        assert!(store.load().unwrap().orchestrator_unreadable.is_empty());
        assert_eq!(
            fs::read_to_string(dir.join("settings.json")).unwrap(),
            older
        );
        // Saving something else writes no orchestrator key: never chosen stays never
        // chosen, so it follows the default of whichever build reads it.
        store
            .update(|settings| settings.dictation_mic = true)
            .unwrap();
        let file = document(&dir);
        for key in [
            "orchestrator_mode",
            "orchestrator_chat_provider",
            "orchestrator_chat_model",
        ] {
            assert!(file.get(key).is_none(), "{key} was written");
        }
        assert_eq!(file["dictation_mic"], true);

        // A saved mode is a choice, whatever the default is now: an older
        // terminal stays a terminal, and an older chat keeps its provider and
        // the provider's own model.
        write(
            r#"{"schema_version":1,"orchestrator_mode":"terminal","orchestrator_chat_provider":"codex"}"#,
        );
        assert_eq!(orchestrator_runs(&dir), OrchestratorRuns::Terminal);
        write(
            r#"{"schema_version":1,"orchestrator_mode":"chat","orchestrator_chat_provider":"codex"}"#,
        );
        assert_eq!(
            orchestrator_runs(&dir),
            OrchestratorRuns::chat(Provider::Codex)
        );
        // Changing something else leaves the choice as it was saved.
        store
            .update(|settings| settings.dictation_mic = true)
            .unwrap();
        let file = document(&dir);
        assert_eq!(file["orchestrator_mode"], "chat");
        assert_eq!(file["orchestrator_chat_provider"], "codex");
        assert!(file.get("orchestrator_chat_model").is_none());
        assert_eq!(
            orchestrator_runs(&dir),
            OrchestratorRuns::chat(Provider::Codex)
        );

        // A chat with a model, chosen and saved; creation sees it at once.
        let claude = OrchestratorRuns::Chat {
            provider: Provider::Claude,
            model: Some("opus".into()),
        };
        store
            .update(|settings| settings.set_orchestrator_runs(&claude))
            .unwrap();
        let file = document(&dir);
        assert_eq!(file["orchestrator_mode"], "chat");
        assert_eq!(file["orchestrator_chat_provider"], "claude");
        assert_eq!(file["orchestrator_chat_model"], "opus");
        assert_eq!(orchestrator_runs(&dir), claude);
        // Automatic and terminal keep the chat's provider and model for later.
        store
            .update(|settings| settings.set_orchestrator_runs(&OrchestratorRuns::Automatic))
            .unwrap();
        assert_eq!(orchestrator_runs(&dir), OrchestratorRuns::Automatic);
        let loaded = store.load().unwrap();
        assert_eq!(loaded.orchestrator_chat_provider, Provider::Claude);
        assert_eq!(loaded.orchestrator_chat_model.as_deref(), Some("opus"));
        assert_eq!(document(&dir)["orchestrator_mode"], "automatic");
        // The provider's own model again is saved as none.
        store
            .update(|settings| {
                settings.set_orchestrator_runs(&OrchestratorRuns::chat(Provider::Muse))
            })
            .unwrap();
        assert_eq!(document(&dir)["orchestrator_chat_model"], Value::Null);
        assert_eq!(
            orchestrator_runs(&dir),
            OrchestratorRuns::chat(Provider::Muse)
        );

        // Values this build cannot read stand as defaults, are named, and stay
        // in the file until the setting is changed.
        write(
            r#"{"schema_version":1,"orchestrator_mode":"hologram","orchestrator_chat_provider":"gemini","orchestrator_chat_model":42}"#,
        );
        let loaded = store.load().unwrap();
        assert_eq!(loaded.orchestrator_runs(), OrchestratorRuns::Automatic);
        assert_eq!(loaded.orchestrator_chat_provider, Provider::Codex);
        assert_eq!(loaded.orchestrator_chat_model, None);
        assert_eq!(
            loaded.orchestrator_unreadable,
            vec![
                (
                    "orchestrator_mode".to_owned(),
                    serde_json::json!("hologram")
                ),
                (
                    "orchestrator_chat_provider".to_owned(),
                    serde_json::json!("gemini")
                ),
                ("orchestrator_chat_model".to_owned(), serde_json::json!(42)),
            ]
        );
        let note = loaded.orchestrator_unreadable_note().unwrap();
        assert!(note.contains(r#"orchestrator_mode = "hologram""#), "{note}");
        assert!(note.contains("orchestrator_chat_model = 42"), "{note}");
        store
            .update(|settings| settings.panel_tab_icons = true)
            .unwrap();
        let file = document(&dir);
        assert_eq!(file["orchestrator_mode"], "hologram");
        assert_eq!(file["orchestrator_chat_provider"], "gemini");
        assert_eq!(file["orchestrator_chat_model"], 42);
        // Choosing the mode Settings show (the default standing in) replaces just it.
        store
            .update(|settings| settings.choose_orchestrator_mode(OrchestratorMode::Automatic))
            .unwrap();
        let file = document(&dir);
        assert_eq!(file["orchestrator_mode"], "automatic");
        assert_eq!(file["orchestrator_chat_provider"], "gemini");
        let loaded = store.load().unwrap();
        assert_eq!(loaded.orchestrator_unreadable.len(), 2);
        assert!(
            !loaded
                .orchestrator_unreadable_note()
                .unwrap()
                .contains("hologram")
        );
        // A model saved beside a provider this build cannot read is not used with the
        // default provider, and is kept too.
        write(
            r#"{"schema_version":1,"orchestrator_mode":"chat","orchestrator_chat_provider":"gemini","orchestrator_chat_model":"muse-spark-1.3"}"#,
        );
        let loaded = store.load().unwrap();
        assert_eq!(
            loaded.orchestrator_runs(),
            OrchestratorRuns::chat(Provider::Codex)
        );
        assert_eq!(
            loaded.orchestrator_unreadable.len(),
            2,
            "provider and its model"
        );
        store
            .update(|settings| settings.panel_tab_icons = true)
            .unwrap();
        assert_eq!(document(&dir)["orchestrator_chat_model"], "muse-spark-1.3");
        // Choosing a provider there, even the one shown, replaces both, so the model is
        // not revived with another provider.
        store
            .update(|settings| settings.choose_orchestrator_provider(Provider::Codex))
            .unwrap();
        let file = document(&dir);
        assert_eq!(file["orchestrator_chat_provider"], "codex");
        assert_eq!(file["orchestrator_chat_model"], Value::Null);
        let loaded = store.load().unwrap();
        assert!(loaded.orchestrator_unreadable.is_empty());
        assert_eq!(
            loaded.orchestrator_runs(),
            OrchestratorRuns::chat(Provider::Codex)
        );
        // A blank or overlong model is no model, and also kept.
        write(r#"{"schema_version":1,"orchestrator_mode":"chat","orchestrator_chat_model":"   "}"#);
        let loaded = store.load().unwrap();
        assert_eq!(
            loaded.orchestrator_runs(),
            OrchestratorRuns::chat(Provider::Codex)
        );
        assert_eq!(loaded.orchestrator_unreadable.len(), 1);
        // Choosing replaces exactly the values chosen.
        store
            .update(|settings| {
                settings.set_orchestrator_runs(&OrchestratorRuns::Chat {
                    provider: Provider::Muse,
                    model: Some("muse-wonder-full-1.0".into()),
                })
            })
            .unwrap();
        let loaded = store.load().unwrap();
        assert!(loaded.orchestrator_unreadable.is_empty());
        assert_eq!(
            document(&dir)["orchestrator_chat_model"],
            "muse-wonder-full-1.0"
        );

        // A damaged file never decides how an orchestrator runs.
        write("{ not json");
        assert_eq!(orchestrator_runs(&dir), OrchestratorRuns::Automatic);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn orchestrator_runs_have_one_shape_on_disk_and_refuse_what_they_do_not_know() {
        let parse = |text: &str| serde_json::from_str::<OrchestratorRuns>(text);
        assert_eq!(
            parse(r#"{"mode":"automatic"}"#).unwrap(),
            OrchestratorRuns::Automatic
        );
        assert_eq!(
            parse(r#"{"mode":"terminal"}"#).unwrap(),
            OrchestratorRuns::Terminal
        );
        assert_eq!(
            parse(r#"{"mode":"chat","provider":"muse","model":"muse-wonder-full-1.0"}"#).unwrap(),
            OrchestratorRuns::Chat {
                provider: Provider::Muse,
                model: Some("muse-wonder-full-1.0".into())
            }
        );
        assert_eq!(
            parse(r#"{"mode":"chat","provider":"codex"}"#).unwrap(),
            OrchestratorRuns::chat(Provider::Codex)
        );
        for odd in [
            r#"{"mode":"hologram"}"#,
            r#"{"mode":"chat"}"#,
            r#"{"mode":"chat","provider":"gemini"}"#,
            r#"{"mode":"chat","provider":"codex","effort":"high"}"#,
            r#"{"mode":"terminal","provider":"codex"}"#,
            r#""automatic""#,
        ] {
            assert!(parse(odd).is_err(), "{odd}");
        }
        assert_eq!(
            serde_json::to_value(OrchestratorRuns::chat(Provider::Claude)).unwrap(),
            serde_json::json!({"mode":"chat","provider":"claude"})
        );
        assert!(
            OrchestratorRuns::Automatic
                .describe()
                .contains("muse-wonder-full-1.0")
        );
        assert!(
            OrchestratorRuns::Automatic
                .describe()
                .contains("gpt-6.1-sol")
        );
        assert!(valid_model_id("gpt-6.1-sol"));
        assert!(!valid_model_id(" "));
        assert!(!valid_model_id("a\nb"));
        assert!(!valid_model_id(&"x".repeat(101)));
    }

    #[test]
    fn ui_text_size_defaults_to_today_clamps_and_keeps_odd_values_until_changed() {
        let dir = env::temp_dir().join(format!("riwork-settings-text-size-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let document = |dir: &std::path::Path| -> Value {
            serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap()
        };
        let defaults = Settings::default();
        assert_eq!(defaults.ui_text_size.points(), 11.0);
        assert!(!defaults.ui_text_matches_terminal);

        // A file from a build without the settings (or with the earlier percentage
        // key) reads as today's size and is not rewritten.
        let older = r#"{"schema_version":1,"theme":"tokyo_night","ui_text_size_percent":140,"future_setting":{"a":1}}"#;
        fs::write(dir.join("settings.json"), older).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.ui_text_size, TextPoints::DEFAULT);
        assert!(!loaded.ui_text_matches_terminal);
        assert_eq!(
            fs::read_to_string(dir.join("settings.json")).unwrap(),
            older
        );

        // A size change steps from the stored 11 pt, writes both keys in points and
        // leaves the neighbours alone.
        let saved = store
            .update(|settings| SizeChange::Bigger.apply(settings, 13.0))
            .unwrap();
        assert_eq!(saved.ui_text_size.points(), 12.0);
        let file = document(&dir);
        assert_eq!(file["ui_text_size"], 12);
        assert_eq!(file["ui_text_matches_terminal"], false);
        assert_eq!(file["theme"], "tokyo_night");
        assert_eq!(file["ui_text_size_percent"], 140);
        assert_eq!(file["future_setting"], serde_json::json!({"a": 1}));
        let reloaded = SettingsStore::open(&dir).unwrap().load().unwrap();
        assert_eq!(reloaded.ui_text_size.points(), 12.0);
        store.update(toggle_text_match).unwrap();
        assert_eq!(document(&dir)["ui_text_matches_terminal"], true);
        assert_eq!(document(&dir)["ui_text_size"], 12);

        // Out of range is pulled into range, a fraction keeps a tenth, another
        // shape is the default. Each stays in the file until changed.
        for (value, expected) in [
            ("500", 24.0),
            ("3", 9.0),
            ("12.5", 12.5),
            ("-120", 9.0),
            (r#""large""#, 11.0),
        ] {
            fs::write(
                dir.join("settings.json"),
                format!(r#"{{"schema_version":1,"ui_text_size":{value},"ui_text_matches_terminal":"yes"}}"#),
            )
            .unwrap();
            let loaded = store.load().unwrap();
            assert_eq!(loaded.ui_text_size.points(), expected, "{value}");
            assert!(!loaded.ui_text_matches_terminal);
            store
                .update(|settings| settings.panel_tab_icons = true)
                .unwrap();
            assert_eq!(
                document(&dir)["ui_text_size"],
                serde_json::from_str::<Value>(value).unwrap()
            );
            assert_eq!(document(&dir)["ui_text_matches_terminal"], "yes");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn each_toggle_changes_only_its_own_setting() {
        let base = Settings::default();
        for (toggle, expected) in [
            (Toggle::TerminalColors, "native_terminal_colors"),
            (Toggle::PanelTabIcons, "panel_tab_icons"),
            (Toggle::PreviewOnSelect, "open_preview_on_select"),
            (Toggle::AgentInline, "agent_inline_mode"),
            (Toggle::AgentsUnrestricted, "agent_terminals_unrestricted"),
            (Toggle::DictationMic, "dictation_mic"),
            (Toggle::WindowSize, "remember_window_size"),
        ] {
            let mut flipped = base.clone();
            toggle.flip(&mut flipped);
            let (before, after) = (
                serde_json::to_value(&base).unwrap(),
                serde_json::to_value(&flipped).unwrap(),
            );
            let changed: Vec<_> = before
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, value)| after[key.as_str()] != **value)
                .map(|(key, _)| key.as_str())
                .collect();
            assert_eq!(changed, [expected]);
            toggle.flip(&mut flipped);
            assert_eq!(flipped, base);
        }
        let toggles = [
            Toggle::TerminalColors,
            Toggle::PanelTabIcons,
            Toggle::PreviewOnSelect,
            Toggle::AgentInline,
            Toggle::AgentsUnrestricted,
            Toggle::DictationMic,
            Toggle::WindowSize,
        ];
        // Every toggle has an id of its own.
        let ids = toggles
            .map(Toggle::id)
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), toggles.len());
    }

    #[test]
    fn concurrent_preferences_merge_and_corrupt_data_is_not_overwritten() {
        let dir = env::temp_dir().join(format!("riwork-settings-test-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        assert_eq!(store.load().unwrap(), Settings::default());
        let other = store.clone();
        let worker = std::thread::spawn(move || {
            other
                .update(|settings| {
                    settings.panel_tab_icons = true;
                    settings.project_order =
                        ProjectOrder::for_sort(crate::project_sort::ProjectSort::LiveSessions);
                })
                .unwrap()
        });
        store
            .update(|settings| settings.remember_window_size = false)
            .unwrap();
        worker.join().unwrap();
        let saved = store.load().unwrap();
        assert!(saved.panel_tab_icons);
        assert!(!saved.remember_window_size);
        assert_eq!(
            saved.project_order,
            ProjectOrder::for_sort(crate::project_sort::ProjectSort::LiveSessions)
        );
        fs::write(dir.join("settings.json"), "broken").unwrap();
        assert!(
            store
                .update(|settings| settings.panel_tab_icons = false)
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(dir.join("settings.json")).unwrap(),
            "broken"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_account_status_and_appearance_updates_preserve_each_other() {
        use crate::status_bar::{StatusItemKind, StatusSide};
        use std::sync::{Arc, Barrier};
        let dir = env::temp_dir().join(format!("riwork-settings-merge-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let mut status = StatusBarSettings::default();
        status.set_side(StatusItemKind::Usage, StatusSide::Left);
        status.set_visible(StatusItemKind::AgentActivity, true);
        status.set_visible(StatusItemKind::Resources, false);
        assert!(status.move_item(StatusItemKind::Usage, true));
        status.enabled = false;
        let barrier = Arc::new(Barrier::new(4));
        let mut workers = Vec::new();
        for index in 0..3 {
            let writer = SettingsStore::open(&dir).unwrap();
            let start = barrier.clone();
            let status = status.clone();
            workers.push(std::thread::spawn(move || {
                start.wait();
                writer
                    .update(|settings| match index {
                        0 => settings.selected_codex_account = Some("other-fixture-account".into()),
                        1 => settings.status_bar = status,
                        _ => {
                            settings.interface_font = InterfaceFont::System;
                            settings.remember_window_size = false;
                        }
                    })
                    .unwrap();
            }));
        }
        barrier.wait();
        let project_order = ProjectOrder::for_sort(crate::project_sort::ProjectSort::LiveSessions);
        store
            .update(|settings| {
                settings.project_order = project_order;
                settings.panel_tab_icons = true;
            })
            .unwrap();
        for worker in workers {
            worker.join().unwrap();
        }
        let loaded = SettingsStore::open(&dir).unwrap().load().unwrap();
        assert_eq!(
            loaded.selected_codex_account.as_deref(),
            Some("other-fixture-account")
        );
        assert_eq!(loaded.status_bar, status);
        assert_eq!(loaded.interface_font, InterfaceFont::System);
        assert!(!loaded.remember_window_size);
        assert!(loaded.panel_tab_icons);
        assert_eq!(loaded.project_order, project_order);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_usage_mode_this_build_cannot_read_reads_as_the_current_chat_and_keeps_the_bar() {
        use crate::status_bar::{StatusItemKind, StatusSide, UsageMode};
        let dir = env::temp_dir().join(format!("riwork-settings-usage-mode-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        // A newer build's mode, beside a bar routed its own way.
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"status_bar":{"enabled":true,"usage_mode":"per_provider","items":[
                {"kind":"usage","enabled":true,"side":"left"},
                {"kind":"codex_account","enabled":false,"side":"right"}]}}"#,
        )
        .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.status_bar.usage_mode, UsageMode::CurrentChat);
        assert_eq!(
            loaded.status_bar.visible_items(StatusSide::Left)[0],
            StatusItemKind::Usage
        );
        assert!(
            !loaded
                .status_bar
                .visible_items(StatusSide::Right)
                .contains(&StatusItemKind::CodexAccount)
        );

        // An unrelated change leaves the newer build's mode in the file.
        store
            .update(|settings| settings.remember_window_size = false)
            .unwrap();
        let file: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(file["status_bar"]["usage_mode"], "per_provider");

        // Choosing All accounts saves it, and the routing chosen before stays.
        store
            .update(|settings| settings.status_bar.usage_mode = UsageMode::AllAccounts)
            .unwrap();
        let reloaded = SettingsStore::open(&dir).unwrap().load().unwrap();
        assert_eq!(reloaded.status_bar.usage_mode, UsageMode::AllAccounts);
        assert_eq!(
            reloaded.status_bar.visible_items(StatusSide::Left)[0],
            StatusItemKind::Usage
        );
        assert!(!reloaded.remember_window_size);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_saved_before_the_layout_menu_show_it_and_keep_a_later_choice_to_hide_it() {
        use crate::status_bar::{StatusItemKind, StatusSide};
        let dir = env::temp_dir().join(format!("riwork-settings-layout-bar-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        // A file as the build before the layout menu wrote it: the old default bar, with the
        // session ID hidden, and an unrelated setting.
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"theme":"tokyo_night","status_bar":{"enabled":true,"items":[
                {"kind":"project","enabled":true,"side":"left"},
                {"kind":"worktree","enabled":false,"side":"left"},
                {"kind":"agent_activity","enabled":false,"side":"left"},
                {"kind":"live_sessions","enabled":true,"side":"right"},
                {"kind":"resources","enabled":true,"side":"right"},
                {"kind":"usage","enabled":true,"side":"right"},
                {"kind":"codex_account","enabled":true,"side":"right"},
                {"kind":"session_id","enabled":false,"side":"right"},
                {"kind":"global_orchestrator","enabled":true,"side":"right"},
                {"kind":"project_orchestrator","enabled":true,"side":"right"}]}}"#,
        )
        .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.theme, ThemeChoice::Native);
        assert_eq!(
            loaded.status_bar.visible_items(StatusSide::Right),
            [
                StatusItemKind::LiveSessions,
                StatusItemKind::Resources,
                StatusItemKind::Usage,
                StatusItemKind::CodexAccount,
                StatusItemKind::GlobalOrchestrator,
                StatusItemKind::ProjectOrchestrator,
                StatusItemKind::Layout
            ]
        );

        // An unrelated change leaves the saved bar as the old build wrote it; the menu is still
        // derived, and shown, each time it is read.
        store
            .update(|settings| settings.remember_window_size = false)
            .unwrap();
        let file: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(file["status_bar"]["items"].as_array().unwrap().len(), 10);
        assert!(
            store
                .load()
                .unwrap()
                .status_bar
                .visible_items(StatusSide::Right)
                .contains(&StatusItemKind::Layout)
        );

        // Changing the bar writes the menu as a choice of its own, so hiding it sticks, and so
        // does where the user then puts it.
        let reopened = SettingsStore::open(&dir).unwrap();
        reopened
            .update(|settings| {
                settings
                    .status_bar
                    .set_visible(StatusItemKind::Layout, false);
            })
            .unwrap();
        let file: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        let layout = file["status_bar"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["kind"] == "layout")
            .expect("the layout menu is saved once the bar is");
        assert_eq!(layout["enabled"], false);
        let hidden = store.load().unwrap();
        assert!(
            !hidden
                .status_bar
                .visible_items(StatusSide::Right)
                .contains(&StatusItemKind::Layout)
        );
        reopened
            .update(|settings| {
                settings
                    .status_bar
                    .set_visible(StatusItemKind::Layout, true);
                settings
                    .status_bar
                    .set_side(StatusItemKind::Layout, StatusSide::Left);
            })
            .unwrap();
        let moved = store.load().unwrap();
        assert_eq!(
            moved.status_bar.visible_items(StatusSide::Left),
            [StatusItemKind::Project, StatusItemKind::Layout]
        );
        assert!(!moved.remember_window_size);
        assert_eq!(moved.theme, ThemeChoice::Native);
        let file: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(file["theme"], "tokyo_night");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_reuses_the_last_parse_until_the_settings_file_changes() {
        use std::io::Write as _;
        let dir = env::temp_dir().join(format!("riwork-settings-cache-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let path = dir.join("settings.json");
        let write_in_place = |bytes: &[u8]| {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)
                .unwrap();
            file.write_all(bytes).unwrap();
        };
        // Pad the shorter encoding with trailing spaces so both files have
        // the same length and only the value differs.
        let bytes = |preview: bool| {
            serde_json::to_vec(
                &serde_json::json!({"schema_version": 1, "open_preview_on_select": preview}),
            )
            .unwrap()
        };
        let (mut on, mut off) = (bytes(true), bytes(false));
        let len = on.len().max(off.len());
        on.resize(len, b' ');
        off.resize(len, b' ');
        write_in_place(&on);
        assert!(store.load().unwrap().open_preview_on_select);
        // Same length, same inode, restored mtime: the cached parse stands.
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        write_in_place(&off);
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert!(store.load().unwrap().open_preview_on_select);
        // The store's own update is visible to the next load.
        store
            .update(|settings| settings.open_preview_on_select = false)
            .unwrap();
        assert!(!store.load().unwrap().open_preview_on_select);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_stores_opened_separately_share_the_last_parse_and_see_every_change() {
        let dir = env::temp_dir().join(format!("riwork-settings-shared-{}", Uuid::new_v4()));
        let path = dir.join("settings.json");
        let preview = || {
            SettingsStore::open(&dir)
                .unwrap()
                .load()
                .unwrap()
                .open_preview_on_select
        };
        let bytes = |preview: bool| {
            let mut bytes = serde_json::to_vec(
                &serde_json::json!({"schema_version": 1, "open_preview_on_select": preview}),
            )
            .unwrap();
            bytes.resize(64, b' ');
            bytes
        };
        SettingsStore::open(&dir).unwrap();
        fs::write(&path, bytes(true)).unwrap();
        assert!(preview());
        // A store opened later is served the first one's parse while the
        // stamp stands.
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, bytes(false)).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert!(preview());
        // One store's update is seen by every other store at once.
        let (first, second) = (
            SettingsStore::open(&dir).unwrap(),
            SettingsStore::open(&dir).unwrap(),
        );
        assert!(first.load().unwrap().open_preview_on_select);
        second
            .update(|settings| settings.open_preview_on_select = false)
            .unwrap();
        assert!(!first.load().unwrap().open_preview_on_select);
        assert!(!preview());
        // So are a change from outside and removal.
        fs::write(&path, bytes(true)).unwrap();
        assert!(preview());
        fs::remove_file(&path).unwrap();
        assert_eq!(
            SettingsStore::open(&dir).unwrap().load().unwrap(),
            Settings::default()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
