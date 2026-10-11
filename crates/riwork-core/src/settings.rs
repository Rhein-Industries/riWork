//! Saved application preferences, shared by every RiWork window and process.
//! UI globals and panels adapt these data types in the desktop crate.

use crate::{
    project_sort::ProjectOrder,
    provider::Provider,
    status_bar::StatusBarSettings,
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
/// How an orchestrator created from now on runs: in a terminal tab, as it always
/// has, or as a chat of the chat host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorMode {
    #[default]
    Terminal,
    Chat,
}

impl OrchestratorMode {
    pub const ALL: [Self; 2] = [Self::Terminal, Self::Chat];

    pub fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Chat => "Chat",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Terminal => Self::Chat,
            Self::Chat => Self::Terminal,
        }
    }
}

/// What an orchestrator created now is: the mode and, for a chat, its agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrchestratorRuns {
    Terminal,
    Chat(Provider),
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
    pub theme: ThemeChoice,
    /// Kept for older preferences and as a terminal-only override while following Ghostty.
    pub use_riwork_colors: bool,
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
    pub status_bar: StatusBarSettings,
    /// New agent sessions (Codex, Grok, Claude Code) run inline on the
    /// terminal's main screen instead of its alternate screen, so the
    /// conversation becomes tmux scrollback that a remote viewer can fetch and
    /// scroll locally. Off leaves every agent on its own default screen. A
    /// session that is already running keeps the mode it started in.
    pub agent_inline_mode: bool,
    /// Codex, Claude and Grok terminals opened from the New tab menu, their shortcuts and
    /// the paired phone start without approval prompts (Codex
    /// `--dangerously-bypass-approvals-and-sandbox`, Claude `--dangerously-skip-permissions`,
    /// Grok `--always-approve`). Off, they ask before they act. A session that is already
    /// running keeps the mode it started in.
    pub agent_terminals_unrestricted: bool,
    /// How orchestrators created from now on run. An orchestrator that exists
    /// keeps its mode until it is closed and created again.
    pub orchestrator_mode: OrchestratorMode,
    /// The agent behind a chat orchestrator (`orchestrator_mode` is `chat`).
    pub orchestrator_chat_provider: Provider,
    /// RiWork's own interface text in points of its panel list text, like
    /// Ghostty's `font-size`: 11 is the design size, from `ui_text::MIN_POINTS`
    /// to `MAX_POINTS`. Terminals keep Ghostty's size.
    pub ui_text_size: TextPoints,
    /// Interface text follows Ghostty's `font-size` instead of `ui_text_size`.
    pub ui_text_matches_terminal: bool,
    /// Native's "Terminals match the theme": terminals take Native's white or
    /// black background and palette. Off keeps Ghostty's own colors.
    pub native_terminal_colors: bool,
    /// Native's interface font. The other themes draw in Menlo.
    pub interface_font: InterfaceFont,
    /// Microphone buttons for dictation: in the Mac's chat message boxes, and on the
    /// paired phone (its chats and terminals), which reads it from `appearance.json` as
    /// `mic`. Off, the Mac's chats show no mic, ⌃⌥D does nothing in them, and no speech
    /// helper runs or asks for a permission.
    pub dictation_mic: bool,
    /// Claude chats start without `ANTHROPIC_API_KEY`, so a key exported in the shell
    /// cannot switch them from the user's Claude subscription to API billing. Off, the
    /// default, runs `claude` as published, with every sign-in method it has; Anthropic's
    /// terms for products that run Claude Code ask that none be removed.
    pub claude_chat_ignores_api_key: bool,
}

impl Settings {
    pub fn chat_display_for(&self, chat_id: Option<&str>) -> DisplayMode {
        chat_id
            .and_then(|id| self.chat_display_modes.get(id))
            .copied()
            .unwrap_or(self.chat_display)
    }
    /// Whether the selected theme's terminal colors option is on: "Terminals
    /// match the theme" for Native, "Use RiWork terminal colors" while following
    /// Ghostty. The presets force their colors whatever this says.
    pub fn terminal_colors_forced(&self) -> bool {
        match self.theme {
            ThemeChoice::Native => self.native_terminal_colors,
            _ => self.use_riwork_colors,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            tab_close_behavior: TabCloseBehavior::Ask,
            chat_display: Default::default(),
            chat_display_modes: BTreeMap::new(),
            theme: ThemeChoice::Ghostty,
            use_riwork_colors: false,
            remember_window_size: true,
            panel_tab_icons: false,
            open_preview_on_select: true,
            project_order: ProjectOrder::default(),
            selected_codex_account: None,
            status_bar: StatusBarSettings::default(),
            agent_inline_mode: true,
            agent_terminals_unrestricted: true,
            orchestrator_mode: OrchestratorMode::default(),
            orchestrator_chat_provider: Provider::Codex,
            ui_text_size: TextPoints::DEFAULT,
            ui_text_matches_terminal: false,
            native_terminal_colors: true,
            interface_font: InterfaceFont::default(),
            dictation_mic: false,
            claude_chat_ignores_api_key: false,
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
            use_riwork_colors: lenient_field(
                &object,
                "use_riwork_colors",
                defaults.use_riwork_colors,
            ),
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
            claude_chat_ignores_api_key: lenient_field(
                &object,
                "claude_chat_ignores_api_key",
                defaults.claude_chat_ignores_api_key,
            ),
        })
    }
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

/// Whether a Codex, Claude or Grok terminal opened now from a menu, a shortcut or the
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

/// Whether a Claude chat started from the state directory `home` loses
/// `ANTHROPIC_API_KEY`, read from the file each time, like `agent_inline_mode`:
/// the chat host outlives the window that changed it. A missing or unreadable
/// file means the default, which leaves the key to `claude`.
pub fn claude_chat_ignores_api_key(home: &Path) -> bool {
    SettingsStore::open(home)
        .and_then(|store| store.load())
        .map_or(
            Settings::default().claude_chat_ignores_api_key,
            |settings| settings.claude_chat_ignores_api_key,
        )
}

/// What an orchestrator created now runs as, read from the file of the state
/// directory `home` each time, like `agent_inline_mode`: the CLI, the MCP tools
/// and every window create orchestrators, and none of them holds a copy that
/// another could outdate. A missing or unreadable file means a terminal.
pub fn orchestrator_runs(home: &Path) -> OrchestratorRuns {
    let settings = SettingsStore::open(home)
        .and_then(|store| store.load())
        .unwrap_or_default();
    match settings.orchestrator_mode {
        OrchestratorMode::Terminal => OrchestratorRuns::Terminal,
        OrchestratorMode::Chat => OrchestratorRuns::Chat(settings.orchestrator_chat_provider),
    }
}

#[derive(Clone)]
pub struct SettingsStore {
    dir: PathBuf,
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
        Ok(Self { dir })
    }

    pub fn load(&self) -> Result<Settings, String> {
        let lock = self.lock_file()?;
        FileExt::lock_shared(&lock).map_err(|error| format!("Cannot lock settings: {error}"))?;
        self.read()
    }

    pub fn update(&self, change: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
        let lock = self.lock_file()?;
        FileExt::lock_exclusive(&lock).map_err(|error| format!("Cannot lock settings: {error}"))?;
        let (before, document) = self.read_with_document()?;
        let mut settings = before.clone();
        change(&mut settings);
        let document = merged_document(document, &before, &settings)?;
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
fn merged_document(
    mut document: Map<String, Value>,
    before: &Settings,
    after: &Settings,
) -> Result<Map<String, Value>, String> {
    let encode = |settings: &Settings| match serde_json::to_value(settings) {
        Ok(Value::Object(fields)) => Ok(fields),
        Ok(_) => Err("Cannot encode settings: not an object".to_owned()),
        Err(error) => Err(format!("Cannot encode settings: {error}")),
    };
    let before = encode(before)?;
    for (key, value) in encode(after)? {
        if document.contains_key(&key) && before.get(&key) == Some(&value) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_text::SizeChange;
    use std::env;

    /// Pure toggle fixtures for the configuration tests; controls live in desktop.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Toggle {
        TerminalColors,
        PanelTabIcons,
        PreviewOnSelect,
        AgentInline,
        AgentsUnrestricted,
        DictationMic,
        ClaudeChatApiKey,
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
                Self::ClaudeChatApiKey => "claude-chat-ignores-api-key",
                Self::WindowSize => "remember-window-size",
            }
        }

        fn flip(self, settings: &mut Settings) {
            let value = match self {
                Self::TerminalColors if settings.theme == ThemeChoice::Native => {
                    &mut settings.native_terminal_colors
                }
                Self::TerminalColors => &mut settings.use_riwork_colors,
                Self::PanelTabIcons => &mut settings.panel_tab_icons,
                Self::PreviewOnSelect => &mut settings.open_preview_on_select,
                Self::AgentInline => &mut settings.agent_inline_mode,
                Self::AgentsUnrestricted => &mut settings.agent_terminals_unrestricted,
                Self::DictationMic => &mut settings.dictation_mic,
                Self::ClaudeChatApiKey => &mut settings.claude_chat_ignores_api_key,
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
        assert_eq!(loaded.theme, ThemeChoice::Ghostty);
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
            .update(|settings| settings.use_riwork_colors = true)
            .unwrap();
        assert!(saved.use_riwork_colors);
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
        assert_eq!(file["use_riwork_colors"], true);

        // A deliberate change replaces exactly that setting.
        store
            .update(|settings| {
                settings.theme = ThemeChoice::Catppuccin;
                settings.remember_window_size = false;
            })
            .unwrap();
        let file = document(&dir);
        assert_eq!(file["theme"], "catppuccin");
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
    fn native_options_default_on_and_the_terminal_row_flips_the_selected_themes_option() {
        let dir = env::temp_dir().join(format!("riwork-settings-native-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let defaults = Settings::default();
        assert!(defaults.native_terminal_colors);
        assert_eq!(defaults.interface_font, InterfaceFont::SystemMono);
        // A file from a build without them reads as the defaults.
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"theme":"native","use_riwork_colors":false}"#,
        )
        .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.theme, ThemeChoice::Native);
        assert!(loaded.native_terminal_colors);
        assert!(loaded.terminal_colors_forced());
        assert_eq!(loaded.interface_font, InterfaceFont::SystemMono);

        // Under Native the terminal row flips Native's option, not RiWork's.
        let saved = store
            .update(|settings| Toggle::TerminalColors.flip(settings))
            .unwrap();
        assert!(!saved.native_terminal_colors);
        assert!(!saved.use_riwork_colors);
        assert!(!saved.terminal_colors_forced());
        let saved = store
            .update(|settings| settings.interface_font = settings.interface_font.other())
            .unwrap();
        assert_eq!(saved.interface_font, InterfaceFont::System);
        let file: Value =
            serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(file["native_terminal_colors"], false);
        assert_eq!(file["interface_font"], "system");
        assert_eq!(store.load().unwrap(), saved);

        // Following Ghostty, the same row is "Use RiWork terminal colors" again.
        let saved = store
            .update(|settings| {
                settings.theme = ThemeChoice::Ghostty;
                Toggle::TerminalColors.flip(settings);
            })
            .unwrap();
        assert!(saved.use_riwork_colors);
        assert!(!saved.native_terminal_colors);
        assert!(saved.terminal_colors_forced());

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
            .update(|settings| settings.use_riwork_colors = true)
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
    fn claude_chats_keep_their_api_key_unless_settings_say_to_ignore_it() {
        let dir = env::temp_dir().join(format!("riwork-settings-api-key-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        assert!(!Settings::default().claude_chat_ignores_api_key);
        // No file, or one from a build without the setting: Claude runs as published.
        assert!(!claude_chat_ignores_api_key(&dir));
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"theme":"native"}"#,
        )
        .unwrap();
        assert!(!claude_chat_ignores_api_key(&dir));

        // Turning it on is seen by the next launch, and off again too.
        store
            .update(|settings| Toggle::ClaudeChatApiKey.flip(settings))
            .unwrap();
        assert!(claude_chat_ignores_api_key(&dir));
        store
            .update(|settings| Toggle::ClaudeChatApiKey.flip(settings))
            .unwrap();
        assert!(!claude_chat_ignores_api_key(&dir));

        // A value of another shape, or an unreadable file, reads as off.
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"claude_chat_ignores_api_key":"yes"}"#,
        )
        .unwrap();
        assert!(!claude_chat_ignores_api_key(&dir));
        fs::write(dir.join("settings.json"), "{not json").unwrap();
        assert!(!claude_chat_ignores_api_key(&dir));
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
    fn orchestrators_run_in_a_terminal_by_default_and_the_choice_survives_older_odd_and_unreadable_files()
     {
        let dir = env::temp_dir().join(format!("riwork-settings-orchestrator-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let document = |dir: &std::path::Path| -> Value {
            serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap()
        };
        let defaults = Settings::default();
        assert_eq!(defaults.orchestrator_mode, OrchestratorMode::Terminal);
        assert_eq!(defaults.orchestrator_chat_provider, Provider::Codex);
        // No file yet: a terminal, and nothing is written.
        assert_eq!(orchestrator_runs(&dir), OrchestratorRuns::Terminal);
        assert!(!dir.join("settings.json").exists());

        // A file from a build without the settings reads as the defaults and is
        // not rewritten.
        let older = r#"{"schema_version":1,"theme":"tokyo_night","future_setting":{"a":1}}"#;
        fs::write(dir.join("settings.json"), older).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.orchestrator_mode, OrchestratorMode::Terminal);
        assert_eq!(loaded.orchestrator_chat_provider, Provider::Codex);
        assert_eq!(orchestrator_runs(&dir), OrchestratorRuns::Terminal);
        assert_eq!(
            fs::read_to_string(dir.join("settings.json")).unwrap(),
            older
        );

        // Choosing chat writes only its keys, and creation sees them at once.
        store
            .update(|settings| settings.orchestrator_mode = OrchestratorMode::Chat)
            .unwrap();
        let file = document(&dir);
        assert_eq!(file["orchestrator_mode"], "chat");
        assert_eq!(file["orchestrator_chat_provider"], "codex");
        assert_eq!(file["theme"], "tokyo_night");
        assert_eq!(file["future_setting"], serde_json::json!({"a": 1}));
        assert_eq!(
            orchestrator_runs(&dir),
            OrchestratorRuns::Chat(Provider::Codex)
        );
        store
            .update(|settings| {
                settings.orchestrator_chat_provider = Provider::Claude;
            })
            .unwrap();
        assert_eq!(document(&dir)["orchestrator_chat_provider"], "claude");
        assert_eq!(
            orchestrator_runs(&dir),
            OrchestratorRuns::Chat(Provider::Claude)
        );
        // The provider only matters for a chat.
        store
            .update(|settings| settings.orchestrator_mode = OrchestratorMode::Terminal)
            .unwrap();
        assert_eq!(orchestrator_runs(&dir), OrchestratorRuns::Terminal);

        // Values in a shape this build lacks read as the defaults and stay until
        // changed.
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"orchestrator_mode":"hologram","orchestrator_chat_provider":"gemini"}"#,
        )
        .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.orchestrator_mode, OrchestratorMode::Terminal);
        assert_eq!(loaded.orchestrator_chat_provider, Provider::Codex);
        store
            .update(|settings| settings.use_riwork_colors = true)
            .unwrap();
        assert_eq!(document(&dir)["orchestrator_mode"], "hologram");
        assert_eq!(document(&dir)["orchestrator_chat_provider"], "gemini");

        // A damaged file never decides how an orchestrator runs.
        fs::write(dir.join("settings.json"), "{ not json").unwrap();
        assert_eq!(orchestrator_runs(&dir), OrchestratorRuns::Terminal);
        fs::remove_dir_all(dir).unwrap();
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
                .update(|settings| settings.use_riwork_colors = true)
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
            (Toggle::TerminalColors, "use_riwork_colors"),
            (Toggle::PanelTabIcons, "panel_tab_icons"),
            (Toggle::PreviewOnSelect, "open_preview_on_select"),
            (Toggle::AgentInline, "agent_inline_mode"),
            (Toggle::AgentsUnrestricted, "agent_terminals_unrestricted"),
            (Toggle::DictationMic, "dictation_mic"),
            (Toggle::ClaudeChatApiKey, "claude_chat_ignores_api_key"),
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
            Toggle::ClaudeChatApiKey,
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
                    settings.use_riwork_colors = true;
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
        assert!(saved.use_riwork_colors);
        assert!(!saved.remember_window_size);
        assert_eq!(
            saved.project_order,
            ProjectOrder::for_sort(crate::project_sort::ProjectSort::LiveSessions)
        );
        fs::write(dir.join("settings.json"), "broken").unwrap();
        assert!(
            store
                .update(|settings| settings.use_riwork_colors = false)
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
                            settings.theme = ThemeChoice::GruvboxLight;
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
                settings.use_riwork_colors = true;
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
        assert_eq!(loaded.theme, ThemeChoice::GruvboxLight);
        assert!(!loaded.remember_window_size);
        assert!(loaded.use_riwork_colors);
        assert_eq!(loaded.project_order, project_order);
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
        assert_eq!(loaded.theme, ThemeChoice::TokyoNight);
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
        assert_eq!(moved.theme, ThemeChoice::TokyoNight);
        fs::remove_dir_all(dir).unwrap();
    }
}
