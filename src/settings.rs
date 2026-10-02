//! Saved application preferences, shared by every RiWork window and process.

use std::{
    cell::Cell,
    collections::BTreeMap,
    fs,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    rc::Rc,
};

use fs2::FileExt;
use gpui::{
    AnyElement, App, Bounds, Context, Div, EventEmitter, FocusHandle, Global, IntoElement,
    KeyDownEvent, MouseButton, Pixels, Render, Window, canvas, div, prelude::*, px, rgb,
};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{
    codex_accounts::{self, AccountsSnapshot},
    cua::{CuaManager, CuaStatus},
    orca_import::{ImportManager, ImportPreview, ImportReceipt},
    project_sort::ProjectOrder,
    remote_service::{self, RemoteState},
    remote_tree::Link,
    status_bar::StatusBarSettings,
    theme::{Appearance, Palette, ThemeChoice, palette},
};

#[derive(Clone, Default)]
pub struct CuaSetupState {
    pub status: Option<CuaStatus>,
    pub pending: Option<&'static str>,
    pub error: Option<String>,
}

impl Global for CuaSetupState {}

#[derive(Clone, Default)]
pub struct CodexAccountsState {
    pub snapshot: Option<AccountsSnapshot>,
    pub pending: bool,
}
impl Global for CodexAccountsState {}

pub fn refresh_codex_accounts(cx: &mut App) {
    if cx.global::<CodexAccountsState>().pending {
        return;
    }
    let mut state = cx.global::<CodexAccountsState>().clone();
    state.pending = true;
    cx.set_global(state);
    let work = cx
        .background_executor()
        .spawn(async { codex_accounts::discover() });
    cx.spawn(async move |cx| {
        let snapshot = work.await;
        cx.update(|cx| {
            cx.set_global(CodexAccountsState {
                snapshot: Some(snapshot),
                pending: false,
            })
        });
    })
    .detach();
}

#[derive(Clone, Copy)]
enum CuaAction {
    Check,
    Install,
    Permissions,
}

pub fn refresh_cua_status(cx: &mut App) {
    run_cua_action(CuaAction::Check, cx);
}

fn run_cua_action(action: CuaAction, cx: &mut App) {
    if cx.global::<CuaSetupState>().pending.is_some() {
        return;
    }
    let mut state = cx.global::<CuaSetupState>().clone();
    state.pending = Some(match action {
        CuaAction::Check => "Checking Cua.ai…",
        CuaAction::Install => "Setting up Cua.ai…",
        CuaAction::Permissions => "Opening macOS access settings…",
    });
    state.error = None;
    cx.set_global(state);
    let task = cx.background_executor().spawn(async move {
        let manager = match CuaManager::open_default() {
            Ok(manager) => manager,
            Err(error) => return (None, Some(error)),
        };
        let result = (|| match action {
            CuaAction::Install => manager.setup(),
            CuaAction::Permissions => manager.request_permissions(),
            CuaAction::Check => {
                if manager.driver_path().is_ok() {
                    manager.ensure_started()?;
                }
                manager.status()
            }
        })();
        match result {
            Ok(status) => (Some(status), None),
            Err(error) => (manager.status().ok(), Some(error)),
        }
    });
    cx.spawn(async move |cx| {
        let (status, error) = task.await;
        cx.update(|cx| {
            let mut state = cx.global::<CuaSetupState>().clone();
            state.pending = None;
            state.status = status;
            state.error = error;
            cx.set_global(state);
        });
    })
    .detach();
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Settings {
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            theme: ThemeChoice::Ghostty,
            use_riwork_colors: false,
            remember_window_size: true,
            panel_tab_icons: false,
            open_preview_on_select: true,
            project_order: ProjectOrder::default(),
            selected_codex_account: None,
            status_bar: StatusBarSettings::default(),
            agent_inline_mode: true,
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

impl Global for Settings {}

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
        document.insert(key, value);
    }
    Ok(document)
}

pub enum SettingsEvent {
    OrcaImported,
    /// Ask for a pairing link and add the Mac it comes from.
    AddHost,
    /// Make a link that lets another Mac control this one.
    PairMac,
}

/// The on/off rows of the panel. Each owns one boolean setting and one focus handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Toggle {
    TerminalColors,
    PanelTabIcons,
    PreviewOnSelect,
    AgentInline,
    WindowSize,
}

impl Toggle {
    fn id(self) -> &'static str {
        match self {
            Self::TerminalColors => "terminal-colors",
            Self::PanelTabIcons => "panel-tab-icons",
            Self::PreviewOnSelect => "open-preview-on-select",
            Self::AgentInline => "agent-inline-mode",
            Self::WindowSize => "remember-window-size",
        }
    }

    fn flip(self, settings: &mut Settings) {
        let value = match self {
            Self::TerminalColors => &mut settings.use_riwork_colors,
            Self::PanelTabIcons => &mut settings.panel_tab_icons,
            Self::PreviewOnSelect => &mut settings.open_preview_on_select,
            Self::AgentInline => &mut settings.agent_inline_mode,
            Self::WindowSize => &mut settings.remember_window_size,
        };
        *value = !*value;
    }
}

pub struct SettingsPanel {
    store: SettingsStore,
    cua_focus: FocusHandle,
    cua_check_focus: FocusHandle,
    account_refresh_focus: FocusHandle,
    account_focus: BTreeMap<String, FocusHandle>,
    theme_focus: Vec<FocusHandle>,
    terminal_focus: FocusHandle,
    tab_icons_focus: FocusHandle,
    preview_focus: FocusHandle,
    inline_focus: FocusHandle,
    size_focus: FocusHandle,
    orca_preview_focus: FocusHandle,
    orca_import_focus: FocusHandle,
    remote_add_focus: FocusHandle,
    remote_pair_focus: FocusHandle,
    /// One per host listed, in the order they are drawn.
    remote_focus: Vec<(String, FocusHandle)>,
    /// The host whose removal waits for a second click.
    remote_confirm: Option<String>,
    remote_pending: Option<&'static str>,
    remote_error: Option<String>,
    orca_preview: Option<ImportPreview>,
    orca_receipt: Option<ImportReceipt>,
    orca_pending: Option<&'static str>,
    orca_error: Option<String>,
    orca_initialized: bool,
    error: Option<String>,
    /// The panel's own bounds, recorded while painting, to choose a width class.
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl EventEmitter<SettingsEvent> for SettingsPanel {}

/// The panel's width class. It is chosen from the panel's own width, not the
/// window's, because Settings can sit in a narrow split beside other tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsLayout {
    /// One full-width column.
    Narrow,
    /// One centred column at a comfortable reading width.
    Medium,
    /// A centred container with two columns of section cards.
    Wide,
}

const MEDIUM_FROM: f32 = 700.0;
const WIDE_FROM: f32 = 1200.0;
/// Descriptions stay readable however wide their card is.
const DESCRIPTION_MAX_WIDTH: f32 = 560.0;
const ROW_GAP: f32 = 8.0;
const ROW_PAD_X: f32 = 12.0;
const ROW_PAD_Y: f32 = 10.0;
/// The narrowest a theme choice can be before the grid drops a column.
const THEME_CELL_MIN_WIDTH: f32 = 220.0;

/// A width that is not measured yet, or not a number, gets the narrow layout.
fn layout_for(width: f32) -> SettingsLayout {
    if width >= WIDE_FROM {
        SettingsLayout::Wide
    } else if width >= MEDIUM_FROM {
        SettingsLayout::Medium
    } else {
        SettingsLayout::Narrow
    }
}

impl SettingsLayout {
    /// The content width limit; a narrow panel simply fills its pane.
    fn max_width(self) -> Option<f32> {
        match self {
            Self::Narrow => None,
            Self::Medium => Some(820.0),
            Self::Wide => Some(1320.0),
        }
    }

    fn page_padding(self) -> f32 {
        match self {
            Self::Narrow => 12.0,
            Self::Medium => 24.0,
            Self::Wide => 28.0,
        }
    }

    fn card_padding(self) -> f32 {
        match self {
            Self::Narrow => 12.0,
            Self::Medium => 16.0,
            Self::Wide => 18.0,
        }
    }

    /// The space between cards, both down a column and between columns.
    fn gap(self) -> f32 {
        match self {
            Self::Narrow => 10.0,
            Self::Medium => 14.0,
            Self::Wide => 16.0,
        }
    }
}

/// The numbered groups of the panel, in the order Tab visits them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Cua,
    Codex,
    Appearance,
    Files,
    Agents,
    Windows,
    StatusBar,
    Remote,
    Orca,
}

impl Section {
    const ALL: [Self; 9] = [
        Self::Cua,
        Self::Codex,
        Self::Appearance,
        Self::Files,
        Self::Agents,
        Self::Windows,
        Self::StatusBar,
        Self::Remote,
        Self::Orca,
    ];

    fn number(self) -> &'static str {
        match self {
            Self::Cua => "01",
            Self::Codex => "02",
            Self::Appearance => "03",
            Self::Files => "04",
            Self::Agents => "05",
            Self::Windows => "06",
            Self::StatusBar => "07",
            Self::Remote => "08",
            Self::Orca => "09",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Cua => "COMPUTER USE",
            Self::Codex => "CODEX ACCOUNTS",
            Self::Appearance => "APPEARANCE",
            Self::Files => "FILES",
            Self::Agents => "AGENT SESSIONS",
            Self::Windows => "WINDOWS",
            Self::StatusBar => "STATUS BAR",
            Self::Remote => "REMOTE",
            Self::Orca => "IMPORT FROM ORCA",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Cua => "Let RiWork agents operate desktop apps through Cua.ai.",
            Self::Codex => "The account new Codex sessions start with.",
            Self::Appearance => "Choose a theme, or sync with Ghostty.",
            Self::Files => "How the Files tree and its Preview pane open together.",
            Self::Agents => "How new Codex, Grok, and Claude sessions use the terminal screen.",
            Self::Windows => "How project windows open.",
            Self::StatusBar => "Choose what appears, which side it sits on, and its order.",
            Self::Remote => {
                "Control other Macs' shells from here, or let another Mac control this one."
            }
            Self::Orca => "Bring projects and worktrees over from Orca once.",
        }
    }
}

/// The sections of each column, top to bottom. The wide layout splits the
/// numbered order in half, so Tab still reads down the left column and then
/// the right, and the two stacks end up about as tall (Computer Use, Codex
/// Accounts, Appearance, Files against Agent sessions, Windows, Status bar,
/// Remote, Import).
fn section_columns(layout: SettingsLayout) -> Vec<Vec<Section>> {
    match layout {
        SettingsLayout::Wide => {
            let (left, right) = Section::ALL.split_at(Section::ALL.len() / 2);
            vec![left.to_vec(), right.to_vec()]
        }
        SettingsLayout::Narrow | SettingsLayout::Medium => vec![Section::ALL.to_vec()],
    }
}

/// The room inside one section card at a given panel width.
fn card_inner_width(layout: SettingsLayout, width: f32) -> f32 {
    let content = (width - 2.0 * layout.page_padding()).max(0.0);
    let content = layout.max_width().map_or(content, |max| content.min(max));
    let columns = section_columns(layout).len() as f32;
    let column = (content - layout.gap() * (columns - 1.0)) / columns;
    // The card's padding plus its one-pixel border on each side.
    (column - 2.0 * (layout.card_padding() + 1.0)).max(0.0)
}

/// Theme choices per row: one on a narrow panel, otherwise as many as fit
/// (two or three) without squeezing their descriptions.
fn theme_columns(layout: SettingsLayout, width: f32) -> usize {
    if layout == SettingsLayout::Narrow {
        return 1;
    }
    let fit = (card_inner_width(layout, width) + ROW_GAP) / (THEME_CELL_MIN_WIDTH + ROW_GAP);
    (fit as usize).clamp(2, 3)
}

/// Everything that changes the arrangement with the panel's width. The panel
/// repaints when this changes, not on every pixel of a resize.
fn width_class(width: f32) -> (SettingsLayout, usize) {
    let layout = layout_for(width);
    (layout, theme_columns(layout, width))
}

fn row_list() -> Div {
    div().flex().flex_col().gap(px(ROW_GAP))
}

/// Button rows sit at the end of their card once there is room for that.
fn action_bar(layout: SettingsLayout) -> Div {
    div()
        .flex()
        .flex_wrap()
        .gap(px(7.0))
        .when(layout != SettingsLayout::Narrow, |bar| bar.justify_end())
}

fn section_card(
    section: Section,
    layout: SettingsLayout,
    colors: Palette,
    body: AnyElement,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .gap(px(12.0))
        .p(px(layout.card_padding()))
        .bg(rgb(colors.panel))
        .border_1()
        .border_color(rgb(colors.divider))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .pb(px(10.0))
                .border_b_1()
                .border_color(rgb(colors.divider))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .text_size(px(11.0))
                        .child(
                            div()
                                .text_color(rgb(colors.magenta))
                                .child(section.number()),
                        )
                        .child(div().text_color(rgb(colors.cyan)).child(section.title())),
                )
                .child(
                    div()
                        .max_w(px(DESCRIPTION_MAX_WIDTH))
                        .text_size(px(10.0))
                        .text_color(rgb(colors.muted))
                        .child(section.description()),
                ),
        )
        .child(body)
        .into_any_element()
}

fn status_chip(label: &'static str, color: u32, colors: Palette) -> AnyElement {
    div()
        .flex_none()
        .px(px(7.0))
        .py(px(3.0))
        .border_1()
        .border_color(rgb(color))
        .bg(rgb(colors.panel_active))
        .text_size(px(9.0))
        .text_color(rgb(color))
        .child(label)
        .into_any_element()
}

fn import_counts(projects: usize, folders: usize, worktrees: usize, colors: Palette) -> AnyElement {
    let mut counts = vec![(projects, "PROJECTS")];
    if folders > 0 {
        counts.push((folders, "FOLDERS"));
    }
    counts.push((worktrees, "WORKTREES"));
    div()
        .flex()
        .flex_wrap()
        .gap(px(7.0))
        .children(counts.into_iter().map(|(count, label)| {
            div()
                .px(px(8.0))
                .py(px(5.0))
                .border_1()
                .border_color(rgb(colors.divider))
                .bg(rgb(colors.panel_active))
                .text_size(px(10.0))
                .text_color(rgb(colors.cyan))
                .child(format!("{count} {label}"))
        }))
        .into_any_element()
}

/// What the Orca section can honestly offer for a preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OrcaImportOffer {
    /// There are records to add.
    Import,
    /// Nothing to add, but skipped-item warnings that a receipt can record.
    Finish,
    /// Nothing to add and nothing to record: importing would only be refused.
    NothingYet,
}

impl OrcaImportOffer {
    fn for_preview(preview: &ImportPreview) -> Self {
        let empty =
            preview.project_count == 0 && preview.folder_count == 0 && preview.worktree_count == 0;
        Self::new(empty, preview.recordable())
    }

    fn new(empty: bool, recordable: bool) -> Self {
        match (empty, recordable) {
            (false, _) => Self::Import,
            (true, true) => Self::Finish,
            (true, false) => Self::NothingYet,
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Import => "These records will be added to RiWork when you import.",
            Self::Finish => "Nothing new to add. Finish to save the one-time import receipt.",
            Self::NothingYet => {
                "Orca has no projects or worktrees to import yet. Once Orca has loaded them, choose PREVIEW IMPORT again."
            }
        }
    }

    fn button(self) -> Option<&'static str> {
        match self {
            Self::Import => Some("IMPORT NOW"),
            Self::Finish => Some("FINISH IMPORT"),
            Self::NothingYet => None,
        }
    }
}

impl SettingsPanel {
    pub fn new(store: SettingsStore, cx: &mut Context<Self>) -> Self {
        let account_ids = cx
            .global::<CodexAccountsState>()
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .accounts
                    .iter()
                    .filter(|account| account.available)
                    .map(|account| account.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        cx.observe_global::<Settings>(|_, cx| cx.notify()).detach();
        cx.observe_global::<Appearance>(|_, cx| cx.notify())
            .detach();
        cx.observe_global::<CuaSetupState>(|_, cx| cx.notify())
            .detach();
        cx.observe_global::<CodexAccountsState>(|view, cx| {
            view.sync_account_focus(cx);
            cx.notify();
        })
        .detach();
        Self {
            store,
            cua_focus: cx.focus_handle(),
            cua_check_focus: cx.focus_handle(),
            account_refresh_focus: cx.focus_handle(),
            account_focus: account_ids
                .into_iter()
                .map(|id| (id, cx.focus_handle()))
                .collect(),
            theme_focus: ThemeChoice::ALL.iter().map(|_| cx.focus_handle()).collect(),
            terminal_focus: cx.focus_handle(),
            tab_icons_focus: cx.focus_handle(),
            preview_focus: cx.focus_handle(),
            inline_focus: cx.focus_handle(),
            size_focus: cx.focus_handle(),
            orca_preview_focus: cx.focus_handle(),
            orca_import_focus: cx.focus_handle(),
            remote_add_focus: cx.focus_handle(),
            remote_pair_focus: cx.focus_handle(),
            remote_focus: Vec::new(),
            remote_confirm: None,
            remote_pending: None,
            remote_error: None,
            orca_preview: None,
            orca_receipt: None,
            orca_pending: None,
            orca_error: None,
            orca_initialized: false,
            error: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
        }
    }

    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        refresh_cua_status(cx);
        self.sync_account_focus(cx);
        if cx.global::<CodexAccountsState>().snapshot.is_none() {
            refresh_codex_accounts(cx);
        }
        if !self.orca_initialized {
            self.preview_orca(cx);
        }
        let settings = cx.global::<Settings>();
        if self
            .focus_order(settings)
            .iter()
            .any(|focus| focus.is_focused(window))
        {
            return;
        }
        if cx
            .global::<CuaSetupState>()
            .status
            .as_ref()
            .is_none_or(|status| !status.ready)
        {
            self.cua_focus.focus(window, cx);
            return;
        }
        let selected = ThemeChoice::ALL
            .iter()
            .position(|theme| *theme == settings.theme)
            .unwrap_or(0);
        self.theme_focus[selected].focus(window, cx);
    }

    fn focus_order(&self, settings: &Settings) -> Vec<FocusHandle> {
        let mut handles = vec![self.cua_focus.clone(), self.cua_check_focus.clone()];
        handles.push(self.account_refresh_focus.clone());
        handles.extend(self.account_focus.values().cloned());
        handles.extend(self.theme_focus.iter().cloned());
        if settings.theme == ThemeChoice::Ghostty {
            handles.push(self.terminal_focus.clone());
        }
        handles.push(self.tab_icons_focus.clone());
        handles.push(self.preview_focus.clone());
        handles.push(self.inline_focus.clone());
        handles.push(self.size_focus.clone());
        handles.push(self.remote_add_focus.clone());
        handles.push(self.remote_pair_focus.clone());
        handles.extend(self.remote_focus.iter().map(|(_, focus)| focus.clone()));
        if self.orca_pending.is_none() {
            handles.push(self.orca_preview_focus.clone());
            if self.can_import_orca() {
                handles.push(self.orca_import_focus.clone());
            }
        }
        handles
    }

    fn change(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        match self.store.update(change) {
            Ok(settings) => {
                cx.set_global(settings);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn sync_account_focus(&mut self, cx: &mut Context<Self>) {
        let ids = cx
            .global::<CodexAccountsState>()
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .accounts
                    .iter()
                    .filter(|account| account.available)
                    .map(|account| account.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.account_focus.retain(|id, _| ids.contains(id));
        for id in ids {
            self.account_focus
                .entry(id)
                .or_insert_with(|| cx.focus_handle());
        }
    }

    fn select_codex_account(&mut self, id: &str, cx: &mut Context<Self>) {
        let account = cx
            .global::<CodexAccountsState>()
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.accounts.iter().find(|account| account.id == id))
            .cloned();
        let Some(account) = account.filter(|account| account.available) else {
            self.error =
                Some("This account is unavailable. Refresh accounts or choose another.".to_owned());
            cx.notify();
            return;
        };
        let selected = (!account.is_system_default).then_some(account.id);
        self.change(
            move |settings| settings.selected_codex_account = selected,
            cx,
        );
    }

    fn codex_accounts_section(&self, layout: SettingsLayout, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let state = cx.global::<CodexAccountsState>().clone();
        let selected = cx.global::<Settings>().selected_codex_account.clone();
        let selected_missing = selected.as_ref().is_some_and(|id| {
            state.snapshot.as_ref().is_some_and(|snapshot| {
                !snapshot
                    .accounts
                    .iter()
                    .any(|account| &account.id == id && account.available)
            })
        });
        let rows = state
            .snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .accounts
                    .iter()
                    .map(|account| {
                        let id = account.id.clone();
                        let focus_id = id.clone();
                        let choose_id = id.clone();
                        let active = if account.is_system_default {
                            selected.is_none()
                        } else {
                            selected.as_deref() == Some(&account.id)
                        };
                        let available = account.available;
                        let focus = self.account_focus.get(&id).cloned();
                        div()
                            .id(format!("codex-account-{id}"))
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .px(px(ROW_PAD_X))
                            .py(px(ROW_PAD_Y))
                            .border_1()
                            .border_color(rgb(if active { colors.cyan } else { colors.divider }))
                            .bg(rgb(if active {
                                colors.panel_active
                            } else {
                                colors.panel
                            }))
                            .when_some(focus, |row, focus| row.track_focus(&focus))
                            .focus_visible(|style| style.border_color(rgb(colors.cyan)))
                            .when(available, |row| {
                                row.cursor_pointer()
                                    .hover(|style| style.bg(rgb(colors.panel_active)))
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.0))
                                    .child(
                                        div()
                                            .text_size(px(12.0))
                                            .text_color(rgb(colors.text))
                                            .text_ellipsis()
                                            .child(account.label.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.0))
                                            .text_color(rgb(colors.muted))
                                            .child(
                                                account.unavailable_reason.clone().unwrap_or_else(
                                                    || {
                                                        if account.is_system_default {
                                                            format!(
                                                                "Current Codex profile · {}",
                                                                codex_accounts::display_home(
                                                                    &account.home,
                                                                ),
                                                            )
                                                        } else if snapshot.source_active_id.as_ref()
                                                            == Some(&account.id)
                                                        {
                                                            "Current Orca account".to_owned()
                                                        } else {
                                                            "Saved in Orca".to_owned()
                                                        }
                                                    },
                                                ),
                                            ),
                                    ),
                            )
                            .child(status_chip(
                                if !available {
                                    "UNAVAILABLE"
                                } else if active {
                                    "SELECTED"
                                } else {
                                    "USE ACCOUNT"
                                },
                                if !available {
                                    colors.gold
                                } else if active {
                                    colors.cyan
                                } else {
                                    colors.muted
                                },
                                colors,
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |view, _, window, cx| {
                                    if let Some(focus) = view.account_focus.get(&focus_id) {
                                        focus.focus(window, cx);
                                    }
                                }),
                            )
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if available {
                                    view.select_codex_account(&choose_id, cx);
                                }
                            }))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let narrow = layout == SettingsLayout::Narrow;
        row_list()
            .child(
                div()
                    .flex()
                    .gap(px(16.0))
                    .when(narrow, |head| head.flex_col())
                    .child(
                        div()
                            .max_w(px(DESCRIPTION_MAX_WIDTH))
                            .text_size(px(10.0))
                            .text_color(rgb(colors.muted))
                            .when(narrow, |note| note.w_full())
                            .when(!narrow, |note| note.flex_1().min_w_0())
                            .child("Choose the account for new Codex sessions. Running sessions keep their account. A project can override this in Project Settings."),
                    )
                    .child(
                        div().id("refresh-codex-accounts").track_focus(&self.account_refresh_focus)
                            .flex_none().px(px(10.0)).py(px(6.0)).self_start()
                            .border_1().border_color(rgb(colors.divider)).cursor_pointer()
                            .text_size(px(10.0)).text_color(rgb(colors.cyan))
                            .hover(|style| style.bg(rgb(colors.panel_active)))
                            .focus_visible(|style| style.bg(rgb(colors.panel_active)).border_color(rgb(colors.cyan)))
                            .child(if state.pending { "CHECKING ACCOUNTS…" } else { "REFRESH ACCOUNTS" })
                            .on_mouse_down(MouseButton::Left, cx.listener(|view, _, window, cx| view.account_refresh_focus.focus(window, cx)))
                            .on_click(cx.listener(|_, _, _, cx| refresh_codex_accounts(cx))),
                    ),
            )
            .children(rows)
            .children(selected_missing.then(|| div().text_size(px(10.0)).text_color(rgb(colors.gold))
                .child("Your selected account is unavailable. Refresh accounts or choose another before starting Codex.")))
            .children(state.snapshot.as_ref().and_then(|snapshot| snapshot.error.as_ref()).map(|error| {
                div().text_size(px(10.0)).text_color(rgb(colors.gold)).child(error.clone())
            }))
            .children(state.snapshot.as_ref().filter(|snapshot| snapshot.from_cache).map(|_| {
                div().text_size(px(10.0)).text_color(rgb(colors.muted)).child("Showing saved accounts while Orca is unavailable.")
            }))
            .into_any_element()
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.platform || event.keystroke.modifiers.control {
            return;
        }
        let settings = cx.global::<Settings>().clone();
        let theme_index = self
            .theme_focus
            .iter()
            .position(|focus| focus.is_focused(window));
        let account_id = self
            .account_focus
            .iter()
            .find(|(_, focus)| focus.is_focused(window))
            .map(|(id, _)| id.clone());
        match event.keystroke.key.as_str() {
            "tab" => {
                let handles = self.focus_order(&settings);
                let current = handles.iter().position(|focus| focus.is_focused(window));
                let next = match current {
                    Some(index) if event.keystroke.modifiers.shift => {
                        (index + handles.len() - 1) % handles.len()
                    }
                    Some(index) => (index + 1) % handles.len(),
                    None if event.keystroke.modifiers.shift => handles.len() - 1,
                    None => 0,
                };
                handles[next].focus(window, cx);
            }
            "up" | "left" | "down" | "right" => {
                let Some(index) = theme_index else {
                    return;
                };
                let next = if matches!(event.keystroke.key.as_str(), "up" | "left") {
                    (index + self.theme_focus.len() - 1) % self.theme_focus.len()
                } else {
                    (index + 1) % self.theme_focus.len()
                };
                self.theme_focus[next].focus(window, cx);
            }
            "space" | "enter" | "return" => {
                if self.cua_focus.is_focused(window) {
                    run_cua_action(Self::primary_cua_action(cx), cx);
                } else if self.cua_check_focus.is_focused(window) {
                    refresh_cua_status(cx);
                } else if self.account_refresh_focus.is_focused(window) {
                    refresh_codex_accounts(cx);
                } else if let Some(account_id) = account_id {
                    self.select_codex_account(&account_id, cx);
                } else if let Some(index) = theme_index {
                    let theme = ThemeChoice::ALL[index];
                    self.change(|settings| settings.theme = theme, cx);
                } else if settings.theme == ThemeChoice::Ghostty
                    && self.terminal_focus.is_focused(window)
                {
                    self.change(|settings| Toggle::TerminalColors.flip(settings), cx);
                } else if self.tab_icons_focus.is_focused(window) {
                    self.change(|settings| Toggle::PanelTabIcons.flip(settings), cx);
                } else if self.preview_focus.is_focused(window) {
                    self.change(|settings| Toggle::PreviewOnSelect.flip(settings), cx);
                } else if self.inline_focus.is_focused(window) {
                    self.change(|settings| Toggle::AgentInline.flip(settings), cx);
                } else if self.size_focus.is_focused(window) {
                    self.change(|settings| Toggle::WindowSize.flip(settings), cx);
                } else if self.remote_add_focus.is_focused(window) {
                    self.remote_confirm = None;
                    cx.emit(SettingsEvent::AddHost);
                } else if self.remote_pair_focus.is_focused(window) {
                    self.remote_confirm = None;
                    cx.emit(SettingsEvent::PairMac);
                } else if let Some(host) = self
                    .remote_focus
                    .iter()
                    .find(|(_, focus)| focus.is_focused(window))
                    .map(|(id, _)| id.clone())
                {
                    self.remove_host(&host, cx);
                } else if self.orca_preview_focus.is_focused(window) {
                    self.preview_orca(cx);
                } else if self.orca_import_focus.is_focused(window) {
                    self.import_orca(cx);
                } else {
                    return;
                }
            }
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    fn can_import_orca(&self) -> bool {
        self.orca_pending.is_none()
            && self.orca_receipt.is_none()
            && self
                .orca_preview
                .as_ref()
                .is_some_and(|preview| preview.already_imported.is_none() && preview.recordable())
    }

    fn preview_orca(&mut self, cx: &mut Context<Self>) {
        if self.orca_pending.is_some() {
            return;
        }
        self.orca_initialized = true;
        self.orca_pending = Some("Reading Orca import preview…");
        self.orca_error = None;
        self.orca_preview = None;
        let work = cx.background_executor().spawn(async move {
            ImportManager::open_default().and_then(|manager| manager.inspect())
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |panel, cx| {
                panel.orca_pending = None;
                match result {
                    Ok(preview) => {
                        panel.orca_receipt = preview.already_imported.clone();
                        panel.orca_preview = Some(preview);
                    }
                    Err(error) => {
                        panel.orca_receipt = None;
                        panel.orca_error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn import_orca(&mut self, cx: &mut Context<Self>) {
        if !self.can_import_orca() {
            return;
        }
        let Some(preview) = self.orca_preview.clone() else {
            return;
        };
        self.orca_pending = Some("Importing Orca projects and worktrees…");
        self.orca_error = None;
        let work = cx.background_executor().spawn(async move {
            ImportManager::open_default().and_then(|manager| manager.import(&preview))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |panel, cx| {
                panel.orca_pending = None;
                match result {
                    Ok(receipt) => {
                        if let Some(preview) = &mut panel.orca_preview {
                            preview.already_imported = Some(receipt.clone());
                        }
                        panel.orca_receipt = Some(receipt);
                        cx.emit(SettingsEvent::OrcaImported);
                    }
                    Err(error) => {
                        panel.orca_preview = None;
                        panel.orca_error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn orca_button(
        &self,
        label: &'static str,
        import: bool,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = palette(cx);
        div()
            .id(if import {
                "orca-import-confirm"
            } else {
                "orca-import-preview"
            })
            .track_focus(if import {
                &self.orca_import_focus
            } else {
                &self.orca_preview_focus
            })
            .px(px(10.0))
            .py(px(6.0))
            .border_1()
            .border_color(rgb(if disabled {
                colors.divider
            } else if import {
                colors.cyan
            } else {
                colors.divider
            }))
            .bg(rgb(if import {
                colors.panel_active
            } else {
                colors.panel
            }))
            .text_size(px(10.0))
            .text_color(rgb(if disabled {
                colors.muted
            } else if import {
                colors.cyan
            } else {
                colors.text
            }))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(colors.panel_active)))
            .focus_visible(|style| style.border_color(rgb(colors.magenta)))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _, window, cx| {
                    if !disabled {
                        if import {
                            &view.orca_import_focus
                        } else {
                            &view.orca_preview_focus
                        }
                        .focus(window, cx);
                    }
                }),
            )
            .on_click(cx.listener(move |view, _, _, cx| {
                if disabled {
                    return;
                }
                if import {
                    view.import_orca(cx);
                } else {
                    view.preview_orca(cx);
                }
            }))
            .into_any_element()
    }

    fn orca_section(&self, layout: SettingsLayout, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let preview = self.orca_preview.as_ref();
        let receipt = self.orca_receipt.as_ref();
        let pending = self.orca_pending.is_some();
        let source = receipt
            .map(|receipt| &receipt.source)
            .or_else(|| preview.map(|preview| &preview.source));
        let counts = receipt
            .map(|receipt| {
                import_counts(
                    receipt.project_count,
                    receipt.folder_count,
                    receipt.worktree_count,
                    colors,
                )
            })
            .or_else(|| {
                preview.map(|preview| {
                    import_counts(
                        preview.project_count,
                        preview.folder_count,
                        preview.worktree_count,
                        colors,
                    )
                })
            });
        let offer = preview.map(OrcaImportOffer::for_preview);
        div()
            .id("orca-import")
            .flex()
            .flex_col()
            .gap(px(9.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().flex_1().text_size(px(13.0)).child("Orca"))
                    .child(status_chip(
                        if pending {
                            "WORKING"
                        } else if receipt.is_some() {
                            "IMPORTED"
                        } else {
                            "ONE-TIME IMPORT"
                        },
                        if receipt.is_some() {
                            colors.cyan
                        } else {
                            colors.magenta
                        },
                        colors,
                    )),
            )
            .child(
                div()
                    .max_w(px(DESCRIPTION_MAX_WIDTH))
                    .text_size(px(11.0))
                    .text_color(rgb(colors.muted))
                    .child(if receipt.is_some() {
                        "Import completed. This receipt is saved across RiWork restarts."
                    } else {
                        "Import projects and local worktrees through Orca CLI. Matching paths are skipped."
                    }),
            )
            .children(source.map(|source| {
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(10.0))
                    .text_color(rgb(colors.muted))
                    .child(format!("ORCA CLI · {}", source.display()))
            }))
            .children(counts)
            .children(offer.filter(|_| receipt.is_none()).map(|offer| {
                div()
                    .max_w(px(DESCRIPTION_MAX_WIDTH))
                    .text_size(px(10.0))
                    .text_color(rgb(colors.muted))
                    .child(offer.hint())
            }))
            .children(preview.filter(|preview| !preview.warnings.is_empty()).map(|preview| {
                div()
                    .id("orca-import-warnings")
                    .max_h(px(120.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .text_size(px(10.0))
                    .text_color(rgb(colors.gold))
                    .children(preview.warnings.iter().cloned().map(|warning| div().child(warning)))
            }))
            .children(self.orca_pending.map(|message| {
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(colors.cyan))
                    .child(message)
            }))
            .children(self.orca_error.as_ref().map(|error| {
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(colors.gold))
                    .child(error.clone())
            }))
            .child(
                action_bar(layout)
                    .child(self.orca_button(
                        if receipt.is_some() {
                            "CHECK IMPORT"
                        } else {
                            "PREVIEW IMPORT"
                        },
                        false,
                        pending,
                        cx,
                    ))
                    .children(
                        offer
                            .filter(|_| receipt.is_none())
                            .and_then(OrcaImportOffer::button)
                            .map(|label| self.orca_button(label, true, !self.can_import_orca(), cx)),
                    ),
            )
            .into_any_element()
    }

    /// Give each host listed a focus handle, in the order they are drawn.
    fn sync_remote_focus(&mut self, cx: &mut Context<Self>) {
        let ids = remote_service::hosts(cx)
            .into_iter()
            .map(|(host, _)| host.id)
            .collect::<Vec<_>>();
        if self.remote_focus.iter().map(|(id, _)| id).eq(ids.iter()) {
            return;
        }
        let mut previous = std::mem::take(&mut self.remote_focus);
        self.remote_focus = ids
            .into_iter()
            .map(|id| {
                let focus = previous
                    .iter()
                    .position(|(old, _)| *old == id)
                    .map(|index| previous.swap_remove(index).1)
                    .unwrap_or_else(|| cx.focus_handle());
                (id, focus)
            })
            .collect();
        if self
            .remote_confirm
            .as_ref()
            .is_some_and(|id| !self.remote_focus.iter().any(|(host, _)| host == id))
        {
            self.remote_confirm = None;
        }
    }

    /// Forget a paired Mac. The first click asks, the second removes: the Mac has to send
    /// a new link to be paired again.
    fn remove_host(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.remote_pending.is_some() {
            return;
        }
        if self.remote_confirm.as_deref() != Some(id) {
            self.remote_confirm = Some(id.to_owned());
            cx.notify();
            return;
        }
        self.remote_confirm = None;
        self.remote_error = None;
        let backend = match cx.global_mut::<RemoteState>().backend() {
            Ok(backend) => backend,
            Err(error) => {
                self.remote_error = Some(error);
                cx.notify();
                return;
            }
        };
        self.remote_pending = Some("Removing…");
        let id = id.to_owned();
        let work = cx
            .background_executor()
            .spawn(async move { backend.remove_host(&id) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |panel, cx| {
                panel.remote_pending = None;
                match result {
                    Ok(()) => {
                        cx.global_mut::<RemoteState>().tree_mut().invalidate_hosts();
                    }
                    Err(error) => panel.remote_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn remote_button(
        &self,
        id: String,
        label: &'static str,
        focus: &FocusHandle,
        accent: u32,
        on_press: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = palette(cx);
        let on_press = Rc::new(on_press);
        let focus_on_press = focus.clone();
        div()
            .id(id)
            .track_focus(focus)
            .px(px(10.0))
            .py(px(6.0))
            .border_1()
            .border_color(rgb(accent))
            .bg(rgb(colors.panel))
            .text_size(px(10.0))
            .text_color(rgb(accent))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(colors.panel_active)))
            .focus_visible(|style| style.border_color(rgb(colors.magenta)))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_, _, window, cx| focus_on_press.focus(window, cx)),
            )
            .on_click(cx.listener(move |view, _, _, cx| on_press(view, cx)))
            .into_any_element()
    }

    fn remote_section(&self, layout: SettingsLayout, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let hosts = remote_service::hosts(cx);
        let error = cx
            .global::<RemoteState>()
            .tree()
            .hosts_error()
            .map(str::to_owned);
        let rows = hosts
            .iter()
            .zip(self.remote_focus.iter())
            .map(|((host, link), (_, focus))| {
                let confirming = self.remote_confirm.as_deref() == Some(host.id.as_str());
                let dot = match link {
                    Some(Link::Online) => colors.cyan,
                    Some(Link::Offline) => colors.muted,
                    Some(Link::Connecting) | None => colors.gold,
                };
                let id = host.id.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(ROW_PAD_X))
                    .py(px(ROW_PAD_Y))
                    .border_1()
                    .border_color(rgb(colors.divider))
                    .bg(rgb(colors.panel_active))
                    .child(div().flex_none().text_color(rgb(dot)).child(
                        if *link == Some(Link::Offline) {
                            "○"
                        } else {
                            "●"
                        },
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_size(px(12.0))
                                    .child(host.label.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(rgb(colors.muted))
                                    .child(link.map_or("Connecting…", Link::text)),
                            ),
                    )
                    .child(self.remote_button(
                        format!("remote-remove-{}", host.id),
                        if confirming {
                            "CONFIRM REMOVE"
                        } else {
                            "REMOVE"
                        },
                        focus,
                        if confirming {
                            colors.gold
                        } else {
                            colors.muted
                        },
                        move |view, cx| view.remove_host(&id, cx),
                        cx,
                    ))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let empty = rows.is_empty();
        div()
            .id("remote-hosts")
            .flex()
            .flex_col()
            .gap(px(9.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().flex_1().text_size(px(13.0)).child("Other Macs"))
                    .child(status_chip(
                        if self.remote_pending.is_some() {
                            "WORKING"
                        } else {
                            "ENCRYPTED"
                        },
                        colors.magenta,
                        colors,
                    )),
            )
            .child(
                div()
                    .max_w(px(DESCRIPTION_MAX_WIDTH))
                    .text_size(px(11.0))
                    .text_color(rgb(colors.muted))
                    .child("Add a Mac to open its projects and shells in this window. A paired Mac has the same control over the other as a paired phone: it can type into any terminal."),
            )
            .child(row_list().children(rows))
            .children(empty.then(|| {
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(colors.muted))
                    .child("No other Mac is paired yet.")
            }))
            .children(error.map(|error| {
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(colors.gold))
                    .child(error)
            }))
            .children(self.remote_pending.map(|message| {
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(colors.cyan))
                    .child(message)
            }))
            .children(self.remote_error.as_ref().map(|error| {
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(colors.gold))
                    .child(error.clone())
            }))
            .child(
                action_bar(layout)
                    .child(self.remote_button(
                        "remote-pair-mac".to_owned(),
                        "PAIR ANOTHER MAC",
                        &self.remote_pair_focus,
                        colors.text,
                        |view, cx| {
                            view.remote_confirm = None;
                            cx.emit(SettingsEvent::PairMac);
                        },
                        cx,
                    ))
                    .child(self.remote_button(
                        "remote-add-host".to_owned(),
                        "ADD HOST",
                        &self.remote_add_focus,
                        colors.cyan,
                        |view, cx| {
                            view.remote_confirm = None;
                            cx.emit(SettingsEvent::AddHost);
                        },
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn primary_cua_action(cx: &App) -> CuaAction {
        match cx.global::<CuaSetupState>().status.as_ref() {
            Some(status) if status.ready => CuaAction::Check,
            Some(status) if status.version.is_none() || !status.running => CuaAction::Install,
            Some(status) if status.installed => CuaAction::Permissions,
            _ => CuaAction::Install,
        }
    }

    fn cua_button(
        &self,
        label: &'static str,
        primary: bool,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = palette(cx);
        div()
            .id(if primary {
                "cua-setup-primary"
            } else {
                "cua-check"
            })
            .track_focus(if primary {
                &self.cua_focus
            } else {
                &self.cua_check_focus
            })
            .px(px(10.0))
            .py(px(6.0))
            .border_1()
            .border_color(rgb(if disabled {
                colors.divider
            } else if primary {
                colors.cyan
            } else {
                colors.divider
            }))
            .bg(rgb(if primary {
                colors.panel_active
            } else {
                colors.panel
            }))
            .text_size(px(10.0))
            .text_color(rgb(if disabled {
                colors.muted
            } else if primary {
                colors.cyan
            } else {
                colors.text
            }))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(colors.panel_active)))
            .focus_visible(|style| style.border_color(rgb(colors.magenta)))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _, window, cx| {
                    if primary {
                        &view.cua_focus
                    } else {
                        &view.cua_check_focus
                    }
                    .focus(window, cx);
                }),
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                if !disabled {
                    let action = if primary {
                        Self::primary_cua_action(cx)
                    } else {
                        CuaAction::Check
                    };
                    run_cua_action(action, cx);
                }
            }))
            .into_any_element()
    }

    fn cua_section(&self, layout: SettingsLayout, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let state = cx.global::<CuaSetupState>().clone();
        let status = state.status.as_ref();
        let installed = status.is_some_and(|status| status.installed);
        let ready = status.is_some_and(|status| status.ready);
        let repair =
            installed && status.is_some_and(|status| status.version.is_none() || !status.running);
        let verify_capture = status.is_some_and(|status| {
            status.accessibility && status.screen_recording && !status.direct_capture_verified
        });
        let message = state
            .pending
            .map(str::to_owned)
            .or_else(|| state.error.clone())
            .or_else(|| status.map(|status| status.message.clone()))
            .unwrap_or_else(|| "Cua.ai controls desktop apps for every RiWork agent.".to_owned());
        let access = status.filter(|status| status.installed).map(|status| {
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.0))
                .child(status_chip(
                    if status.accessibility {
                        "ACCESSIBILITY READY"
                    } else {
                        "ACCESSIBILITY NEEDED"
                    },
                    if status.accessibility {
                        colors.cyan
                    } else {
                        colors.gold
                    },
                    colors,
                ))
                .child(status_chip(
                    if status.screen_recording {
                        "RECORDING READY"
                    } else {
                        "RECORDING NEEDED"
                    },
                    if status.screen_recording {
                        colors.cyan
                    } else {
                        colors.gold
                    },
                    colors,
                ))
                .child(status_chip(
                    if status.direct_capture_verified {
                        "CAPTURE VERIFIED"
                    } else {
                        "CAPTURE UNVERIFIED"
                    },
                    if status.direct_capture_verified {
                        colors.cyan
                    } else {
                        colors.gold
                    },
                    colors,
                ))
        });
        div()
            .id("cua-setup")
            .flex()
            .flex_col()
            .gap(px(9.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().flex_1().text_size(px(13.0)).child("Cua.ai"))
                    .child(status_chip(
                        if state.pending.is_some() { "WORKING" } else if ready { "CONNECTED" } else { "SETUP NEEDED" },
                        if ready { colors.cyan } else { colors.gold },
                        colors,
                    )),
            )
            .child(div().max_w(px(DESCRIPTION_MAX_WIDTH)).text_size(px(11.0)).text_color(rgb(if state.error.is_some() { colors.gold } else { colors.muted })).child(message))
            .children(access)
            .children((installed && !ready && !verify_capture).then(|| div().max_w(px(DESCRIPTION_MAX_WIDTH)).text_size(px(11.0)).text_color(rgb(colors.muted))
                .child("Enable CuaDriver in macOS Accessibility and Screen Recording to connect all agents.")))
            .child(div().max_w(px(DESCRIPTION_MAX_WIDTH)).text_size(px(10.0)).text_color(rgb(colors.muted)).child("New agent sessions connect automatically. Restart existing sessions to connect them."))
            .child(action_bar(layout)
                .child(self.cua_button(if ready { "CHECK CUA" } else if repair { "REPAIR CUA" } else if verify_capture { "VERIFY SCREEN CAPTURE" } else if installed { "GRANT MACOS ACCESS" } else { "SET UP CUA" }, true, state.pending.is_some(), cx))
                .child(self.cua_button("CHECK AGAIN", false, state.pending.is_some(), cx)))
            .into_any_element()
    }

    fn theme_row(
        &self,
        index: usize,
        theme: ThemeChoice,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = palette(cx);
        div()
            .id(("theme-choice", index))
            .track_focus(&self.theme_focus[index])
            .flex_1()
            .min_w_0()
            .flex()
            .items_start()
            .gap(px(12.0))
            .px(px(ROW_PAD_X))
            .py(px(ROW_PAD_Y))
            .bg(rgb(if selected {
                colors.panel_active
            } else {
                colors.panel
            }))
            .border_1()
            .border_color(rgb(if selected {
                colors.cyan
            } else {
                colors.divider
            }))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(colors.panel_active)))
            .focus_visible(|style| style.border_color(rgb(colors.magenta)))
            .child(
                div()
                    .flex_none()
                    .mt(px(4.0))
                    .size(px(8.0))
                    .border_1()
                    .border_color(rgb(if selected { colors.cyan } else { colors.muted }))
                    .when(selected, |style| style.bg(rgb(colors.cyan))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        // The chip shares the title's line so a grid cell keeps its width for the text.
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(12.0))
                                    .text_color(rgb(colors.text))
                                    .child(theme.label()),
                            )
                            .children(selected.then(|| status_chip("ACTIVE", colors.cyan, colors))),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(colors.muted))
                            .child(theme.description()),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _, window, cx| {
                    view.theme_focus[index].focus(window, cx);
                }),
            )
            .on_click(cx.listener(move |view, _, _, cx| {
                view.change(|settings| settings.theme = theme, cx);
            }))
            .into_any_element()
    }

    /// The theme choices in rows of `columns` equal cells; a short last row keeps its cell widths.
    fn theme_grid(&self, columns: usize, selected: ThemeChoice, cx: &mut Context<Self>) -> Div {
        let mut cells = ThemeChoice::ALL
            .iter()
            .copied()
            .enumerate()
            .map(|(index, theme)| self.theme_row(index, theme, selected == theme, cx))
            .collect::<Vec<_>>()
            .into_iter()
            .peekable();
        let mut grid = row_list();
        while cells.peek().is_some() {
            let mut row = div().flex().gap(px(ROW_GAP));
            for _ in 0..columns {
                let cell = div().flex_1().min_w_0().flex();
                row = row.child(match cells.next() {
                    Some(theme) => cell.child(theme),
                    None => cell,
                });
            }
            grid = grid.child(row);
        }
        grid
    }

    fn toggle_row(
        &self,
        toggle: Toggle,
        title: &'static str,
        description: &'static str,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = palette(cx);
        let focus = match toggle {
            Toggle::TerminalColors => &self.terminal_focus,
            Toggle::PanelTabIcons => &self.tab_icons_focus,
            Toggle::PreviewOnSelect => &self.preview_focus,
            Toggle::AgentInline => &self.inline_focus,
            Toggle::WindowSize => &self.size_focus,
        };
        div()
            .id(toggle.id())
            .track_focus(focus)
            .flex()
            .items_center()
            .gap(px(12.0))
            .px(px(ROW_PAD_X))
            .py(px(ROW_PAD_Y))
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.divider))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(colors.panel_active)))
            .focus_visible(|style| style.border_color(rgb(colors.cyan)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(colors.text))
                            .child(title),
                    )
                    .child(
                        div()
                            .max_w(px(DESCRIPTION_MAX_WIDTH))
                            .text_size(px(10.0))
                            .text_color(rgb(colors.muted))
                            .child(description),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(42.0))
                    .py(px(4.0))
                    .border_1()
                    .border_color(rgb(if enabled { colors.cyan } else { colors.divider }))
                    .bg(rgb(colors.panel_active))
                    .text_color(rgb(if enabled { colors.cyan } else { colors.muted }))
                    .text_size(px(10.0))
                    .text_center()
                    .child(if enabled { "ON" } else { "OFF" }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, _, window, cx| {
                    match toggle {
                        Toggle::TerminalColors => &view.terminal_focus,
                        Toggle::PanelTabIcons => &view.tab_icons_focus,
                        Toggle::PreviewOnSelect => &view.preview_focus,
                        Toggle::AgentInline => &view.inline_focus,
                        Toggle::WindowSize => &view.size_focus,
                    }
                    .focus(window, cx);
                }),
            )
            .on_click(cx.listener(move |view, _, _, cx| {
                view.change(|settings| toggle.flip(settings), cx);
            }))
            .into_any_element()
    }

    fn appearance_section(
        &self,
        theme_columns: usize,
        settings: &Settings,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = palette(cx);
        let appearance_error = cx.global::<Appearance>().error.clone();
        let terminal_row = (settings.theme == ThemeChoice::Ghostty).then(|| {
            self.toggle_row(
                Toggle::TerminalColors,
                "Use RiWork terminal colors",
                "Keep RiWork terminal colors while following Ghostty. Off uses Ghostty colors.",
                settings.use_riwork_colors,
                cx,
            )
        });
        row_list()
            .child(self.theme_grid(theme_columns, settings.theme, cx))
            .children(appearance_error.map(|error| div().text_size(px(11.0)).text_color(rgb(colors.gold)).child(error)))
            .children(terminal_row)
            .child(self.toggle_row(
                Toggle::PanelTabIcons,
                "Icons instead of labels",
                "Show icons instead of words on the panel tabs and on toolbar buttons, such as the create buttons in Projects and the file actions in Files. Hover an icon for its name.",
                settings.panel_tab_icons,
                cx,
            ))
            .into_any_element()
    }

    fn section_body(
        &self,
        section: Section,
        layout: SettingsLayout,
        theme_columns: usize,
        settings: &Settings,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match section {
            Section::Cua => self.cua_section(layout, cx),
            Section::Codex => self.codex_accounts_section(layout, cx),
            Section::Appearance => self.appearance_section(theme_columns, settings, cx),
            Section::Files => self.toggle_row(
                Toggle::PreviewOnSelect,
                "Open the preview when a file is selected",
                "Selecting a file in Files opens the Preview in a pane of its own, or brings it forward if it is behind another tab. Off keeps a closed preview closed; open it from a pane's menu or with Cmd+Shift+P.",
                settings.open_preview_on_select,
                cx,
            ),
            Section::Agents => self.toggle_row(
                Toggle::AgentInline,
                "Keep agent transcripts in scrollback (inline mode)",
                "New Codex, Grok, and Claude sessions draw on the terminal's main screen, so the whole conversation stays in the terminal's scrollback. You can scroll it locally, and the iOS app can download and scroll it without sending keys to the agent. Off runs them full screen. A session that is already running keeps its mode until it restarts.",
                settings.agent_inline_mode,
                cx,
            ),
            Section::Windows => self.toggle_row(
                Toggle::WindowSize,
                "Remember project window size",
                "Reopen projects at their last size. Switching projects keeps the current window size.",
                settings.remember_window_size,
                cx,
            ),
            Section::StatusBar => crate::status_bar::render_settings(&settings.status_bar, |view: &mut Self, status, _, cx| {
                view.change(move |settings| settings.status_bar = status, cx);
            }, cx),
            Section::Remote => self.remote_section(layout, cx),
            Section::Orca => self.orca_section(layout, cx),
        }
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_remote_focus(cx);
        let settings = cx.global::<Settings>().clone();
        let colors = palette(cx);
        let width = self.bounds.get().size.width.as_f32();
        let (layout, theme_cells) = width_class(width);
        let mut columns = Vec::new();
        for sections in section_columns(layout) {
            let mut cards = Vec::new();
            for section in sections {
                let body = self.section_body(section, layout, theme_cells, &settings, cx);
                cards.push(section_card(section, layout, colors, body));
            }
            columns.push(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(layout.gap()))
                    .children(cards),
            );
        }
        let bounds = self.bounds.clone();
        let measured_view = cx.entity();
        div()
            .id("settings-panel")
            .key_context("RiWorkSettings")
            .on_key_down(cx.listener(Self::key_down))
            .relative()
            .size_full()
            .bg(rgb(colors.bg))
            .child(
                canvas(
                    move |measured, _, cx| {
                        let previous = bounds.replace(measured);
                        if width_class(previous.size.width.as_f32())
                            != width_class(measured.size.width.as_f32())
                        {
                            let view = measured_view.clone();
                            cx.defer(move |cx| {
                                view.update(cx, |_, cx| cx.notify());
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .id("settings-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .justify_center()
                            .p(px(layout.page_padding()))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .w_full()
                                    .min_w_0()
                                    .when_some(layout.max_width(), |page, max| page.max_w(px(max)))
                                    .gap(px(layout.gap()))
                                    .font_family("Menlo")
                                    .text_color(rgb(colors.text))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(12.0))
                                            .pb(px(14.0))
                                            .border_b_1()
                                            .border_color(rgb(colors.cyan))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex()
                                                    .flex_col()
                                                    .gap(px(4.0))
                                                    .child(div().text_size(px(10.0)).text_color(rgb(colors.magenta)).child("RIWORK / PREFERENCES"))
                                                    .child(div().text_size(px(20.0)).child("Settings")),
                                            )
                                            .child(status_chip("ALL PROJECTS", colors.magenta, colors)),
                                    )
                                    // Columns only differ in the wide layout; otherwise this is one stack of cards.
                                    .child(div().flex().items_start().gap(px(layout.gap())).children(columns))
                                    .children(self.error.as_ref().map(|error| div().text_size(px(11.0)).text_color(rgb(colors.gold)).child(error.clone())))
                                    .child(div().pt(px(10.0)).border_t_1().border_color(rgb(colors.divider)).text_size(px(10.0)).text_color(rgb(colors.muted)).child("Saved automatically · Tab to move · Enter or Space to select")),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn missing_and_older_settings_preserve_terminal_colors_by_default() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.theme, ThemeChoice::Ghostty);
        assert!(!settings.use_riwork_colors);
        assert!(settings.remember_window_size);
        assert_eq!(settings.project_order, ProjectOrder::default());
        assert_eq!(settings.selected_codex_account, None);
        assert_eq!(settings.status_bar, StatusBarSettings::default());
        let legacy: Settings =
            serde_json::from_str(r#"{"schema_version":1,"use_riwork_colors":true}"#).unwrap();
        assert_eq!(legacy.theme, ThemeChoice::Ghostty);
        assert!(legacy.use_riwork_colors);
        assert_eq!(legacy.selected_codex_account, None);
        assert_eq!(legacy.status_bar, StatusBarSettings::default());
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
    fn panel_tab_icons_default_off_for_older_files_and_round_trip_beside_other_keys() {
        let dir = env::temp_dir().join(format!("riwork-settings-tab-icons-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let document = |dir: &std::path::Path| -> Value {
            serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap()
        };
        assert!(!Settings::default().panel_tab_icons);

        // A file from a build without the setting reads as off and is not rewritten by a load.
        let older = r#"{"schema_version":1,"theme":"tokyo_night","remember_window_size":false,"future_setting":{"a":1}}"#;
        fs::write(dir.join("settings.json"), older).unwrap();
        assert!(!store.load().unwrap().panel_tab_icons);
        assert_eq!(
            fs::read_to_string(dir.join("settings.json")).unwrap(),
            older
        );

        // Turning it on writes the key and leaves the neighbouring and unknown keys alone.
        let saved = store
            .update(|settings| Toggle::PanelTabIcons.flip(settings))
            .unwrap();
        assert!(saved.panel_tab_icons);
        let file = document(&dir);
        assert_eq!(file["panel_tab_icons"], true);
        assert_eq!(file["theme"], "tokyo_night");
        assert_eq!(file["remember_window_size"], false);
        assert_eq!(file["future_setting"], serde_json::json!({"a": 1}));
        let reloaded = SettingsStore::open(&dir).unwrap().load().unwrap();
        assert!(reloaded.panel_tab_icons);
        assert_eq!(reloaded.theme, ThemeChoice::TokyoNight);
        assert!(!reloaded.remember_window_size);
        let encoded = serde_json::to_string(&reloaded).unwrap();
        assert_eq!(
            serde_json::from_str::<Settings>(&encoded).unwrap(),
            reloaded
        );

        // Other settings changes keep it, and it turns off again.
        store
            .update(|settings| settings.use_riwork_colors = true)
            .unwrap();
        assert_eq!(document(&dir)["panel_tab_icons"], true);
        store
            .update(|settings| Toggle::PanelTabIcons.flip(settings))
            .unwrap();
        assert_eq!(document(&dir)["panel_tab_icons"], false);
        assert!(!store.load().unwrap().panel_tab_icons);

        // A value another build wrote in a shape this one lacks reads as off and stays until changed.
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"panel_tab_icons":"labels"}"#,
        )
        .unwrap();
        assert!(!store.load().unwrap().panel_tab_icons);
        store
            .update(|settings| settings.use_riwork_colors = true)
            .unwrap();
        assert_eq!(document(&dir)["panel_tab_icons"], "labels");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_preview_on_select_defaults_on_and_a_closed_preview_stays_closed_when_off() {
        let dir = env::temp_dir().join(format!("riwork-settings-preview-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let document = |dir: &std::path::Path| -> Value {
            serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap()
        };
        assert!(Settings::default().open_preview_on_select);

        // A file from a build without the setting reads as on and is not rewritten by a load.
        let older = r#"{"schema_version":1,"theme":"tokyo_night","panel_tab_icons":true,"future_setting":{"a":1}}"#;
        fs::write(dir.join("settings.json"), older).unwrap();
        assert!(store.load().unwrap().open_preview_on_select);
        assert_eq!(
            fs::read_to_string(dir.join("settings.json")).unwrap(),
            older
        );

        // Turning it off writes only that key and leaves its neighbours alone.
        let saved = store
            .update(|settings| Toggle::PreviewOnSelect.flip(settings))
            .unwrap();
        assert!(!saved.open_preview_on_select);
        let file = document(&dir);
        assert_eq!(file["open_preview_on_select"], false);
        assert_eq!(file["panel_tab_icons"], true);
        assert_eq!(file["future_setting"], serde_json::json!({"a": 1}));
        let reloaded = SettingsStore::open(&dir).unwrap().load().unwrap();
        assert!(!reloaded.open_preview_on_select);
        store
            .update(|settings| settings.use_riwork_colors = true)
            .unwrap();
        assert_eq!(document(&dir)["open_preview_on_select"], false);

        // A value in a shape this build lacks reads as on and stays until changed.
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"open_preview_on_select":"sometimes"}"#,
        )
        .unwrap();
        assert!(store.load().unwrap().open_preview_on_select);
        store
            .update(|settings| settings.use_riwork_colors = true)
            .unwrap();
        assert_eq!(document(&dir)["open_preview_on_select"], "sometimes");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn agent_inline_mode_defaults_on_and_survives_older_odd_and_unreadable_files() {
        let dir = env::temp_dir().join(format!("riwork-settings-inline-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        let document = |dir: &std::path::Path| -> Value {
            serde_json::from_slice(&fs::read(dir.join("settings.json")).unwrap()).unwrap()
        };
        assert!(Settings::default().agent_inline_mode);
        // No file yet: the default applies to a launch, and nothing is written.
        assert!(agent_inline_mode(&dir));
        assert!(!dir.join("settings.json").exists());

        // A file from a build without the setting reads as on and is not rewritten.
        let older = r#"{"schema_version":1,"theme":"tokyo_night","future_setting":{"a":1}}"#;
        fs::write(dir.join("settings.json"), older).unwrap();
        assert!(store.load().unwrap().agent_inline_mode);
        assert!(agent_inline_mode(&dir));
        assert_eq!(
            fs::read_to_string(dir.join("settings.json")).unwrap(),
            older
        );

        // Turning it off writes only that key, and launches see it at once.
        let saved = store
            .update(|settings| Toggle::AgentInline.flip(settings))
            .unwrap();
        assert!(!saved.agent_inline_mode);
        let file = document(&dir);
        assert_eq!(file["agent_inline_mode"], false);
        assert_eq!(file["theme"], "tokyo_night");
        assert_eq!(file["future_setting"], serde_json::json!({"a": 1}));
        assert!(!agent_inline_mode(&dir));
        store
            .update(|settings| settings.use_riwork_colors = true)
            .unwrap();
        assert_eq!(document(&dir)["agent_inline_mode"], false);
        store
            .update(|settings| Toggle::AgentInline.flip(settings))
            .unwrap();
        assert!(agent_inline_mode(&dir));

        // A value in a shape this build lacks reads as on and stays until changed.
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"agent_inline_mode":"minimal"}"#,
        )
        .unwrap();
        assert!(store.load().unwrap().agent_inline_mode);
        store
            .update(|settings| settings.use_riwork_colors = true)
            .unwrap();
        assert_eq!(document(&dir)["agent_inline_mode"], "minimal");

        // A damaged file does not decide how an agent draws.
        fs::write(dir.join("settings.json"), "{ not json").unwrap();
        assert!(agent_inline_mode(&dir));
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
        let ids = [
            Toggle::TerminalColors,
            Toggle::PanelTabIcons,
            Toggle::PreviewOnSelect,
            Toggle::AgentInline,
            Toggle::WindowSize,
        ]
        .map(Toggle::id)
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 5);
    }

    #[test]
    fn each_theme_round_trips_and_persists_without_losing_other_preferences() {
        let dir = env::temp_dir().join(format!("riwork-themes-test-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        store
            .update(|settings| {
                settings.use_riwork_colors = true;
                settings.remember_window_size = false;
                settings.project_order =
                    ProjectOrder::for_sort(crate::project_sort::ProjectSort::Name).toggled();
            })
            .unwrap();
        for theme in ThemeChoice::ALL.iter().copied() {
            let saved = store.update(|settings| settings.theme = theme).unwrap();
            let encoded = serde_json::to_string(&saved).unwrap();
            assert_eq!(serde_json::from_str::<Settings>(&encoded).unwrap(), saved);
            let reloaded = SettingsStore::open(&dir).unwrap().load().unwrap();
            assert_eq!(reloaded.theme, theme);
            assert!(reloaded.use_riwork_colors);
            assert!(!reloaded.remember_window_size);
            assert_eq!(
                reloaded.project_order,
                ProjectOrder::for_sort(crate::project_sort::ProjectSort::Name).toggled()
            );
        }
        fs::remove_dir_all(dir).unwrap();
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
    fn account_and_status_preferences_upgrade_and_persist_across_reopens() {
        use crate::status_bar::{StatusItemKind, StatusSide};
        let dir = env::temp_dir().join(format!("riwork-settings-account-bar-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"theme":"tokyo_night","remember_window_size":false,"project_order":{"by":"name","descending":false}}"#,
        )
        .unwrap();
        let older = store.load().unwrap();
        assert_eq!(older.theme, ThemeChoice::TokyoNight);
        assert_eq!(older.selected_codex_account, None);
        assert_eq!(older.status_bar, StatusBarSettings::default());

        let mut status = StatusBarSettings::default();
        status.set_side(StatusItemKind::Project, StatusSide::Right);
        status.set_visible(StatusItemKind::Worktree, true);
        status.set_visible(StatusItemKind::SessionId, false);
        assert!(status.move_item(StatusItemKind::Usage, true));
        let saved = store
            .update(|settings| {
                settings.selected_codex_account = Some("fixture-account".into());
                settings.status_bar = status.clone();
            })
            .unwrap();
        assert_eq!(saved.theme, ThemeChoice::TokyoNight);
        assert!(!saved.remember_window_size);
        assert_eq!(saved.project_order, older.project_order);

        let reopened = SettingsStore::open(&dir).unwrap();
        let loaded = reopened.load().unwrap();
        assert_eq!(
            loaded.selected_codex_account.as_deref(),
            Some("fixture-account")
        );
        assert_eq!(loaded.status_bar, status);
        reopened
            .update(|settings| settings.theme = ThemeChoice::Catppuccin)
            .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.theme, ThemeChoice::Catppuccin);
        assert_eq!(
            loaded.selected_codex_account.as_deref(),
            Some("fixture-account")
        );
        assert_eq!(loaded.status_bar, status);

        reopened
            .update(|settings| settings.selected_codex_account = None)
            .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.selected_codex_account, None);
        assert_eq!(loaded.status_bar, status);
        assert_eq!(loaded.theme, ThemeChoice::Catppuccin);
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
    fn partial_status_preferences_do_not_reset_other_saved_settings() {
        use crate::status_bar::{StatusItemKind, StatusSide};
        let dir = env::temp_dir().join(format!("riwork-settings-partial-bar-{}", Uuid::new_v4()));
        let store = SettingsStore::open(&dir).unwrap();
        fs::write(
            dir.join("settings.json"),
            r#"{"theme":"ri_work","selected_codex_account":"saved-fixture-account","status_bar":{"items":[{"kind":"project"},{"kind":"future_widget","enabled":true}]}}"#,
        ).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.theme, ThemeChoice::RiWork);
        assert_eq!(
            loaded.selected_codex_account.as_deref(),
            Some("saved-fixture-account")
        );
        assert_eq!(
            loaded.status_bar.visible_items(StatusSide::Left),
            [StatusItemKind::Project]
        );
        assert_eq!(
            loaded.status_bar.visible_items(StatusSide::Right),
            [StatusItemKind::CodexAccount]
        );
        store
            .update(|settings| settings.remember_window_size = false)
            .unwrap();
        let reloaded = SettingsStore::open(&dir).unwrap().load().unwrap();
        assert_eq!(reloaded.status_bar, loaded.status_bar);
        assert_eq!(
            reloaded.selected_codex_account,
            loaded.selected_codex_account
        );
        assert_eq!(reloaded.theme, ThemeChoice::RiWork);
        assert!(!reloaded.remember_window_size);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_empty_orca_plan_offers_finish_only_when_a_receipt_has_something_to_record() {
        // Nothing found and nothing skipped: import refuses, so no button.
        let nothing = OrcaImportOffer::new(true, false);
        assert_eq!(nothing, OrcaImportOffer::NothingYet);
        assert_eq!(nothing.button(), None);
        assert!(
            nothing
                .hint()
                .starts_with("Orca has no projects or worktrees")
        );
        // Skipped-item warnings are still recorded as a receipt.
        let skipped = OrcaImportOffer::new(true, true);
        assert_eq!(skipped, OrcaImportOffer::Finish);
        assert_eq!(skipped.button(), Some("FINISH IMPORT"));
        assert!(skipped.hint().starts_with("Nothing new to add"));
        let records = OrcaImportOffer::new(false, true);
        assert_eq!(records, OrcaImportOffer::Import);
        assert_eq!(records.button(), Some("IMPORT NOW"));
    }

    #[test]
    fn width_classes_switch_at_the_documented_breakpoints() {
        use SettingsLayout::{Medium, Narrow, Wide};
        for (width, expected) in [
            (0.0, Narrow),
            (300.0, Narrow),
            (699.9, Narrow),
            (700.0, Medium),
            (820.0, Medium),
            (1199.9, Medium),
            (1200.0, Wide),
            (2400.0, Wide),
            // Not measured yet, or not a usable width: fall back to one column.
            (-50.0, Narrow),
            (f32::NAN, Narrow),
        ] {
            assert_eq!(layout_for(width), expected, "{width}");
        }
        assert_eq!(Narrow.max_width(), None);
        assert_eq!(Medium.max_width(), Some(820.0));
        assert_eq!(Wide.max_width(), Some(1320.0));
    }

    #[test]
    fn every_section_lands_in_exactly_one_column_in_tab_order() {
        use SettingsLayout::{Medium, Narrow, Wide};
        for (layout, column_count) in [(Narrow, 1), (Medium, 1), (Wide, 2)] {
            let columns = section_columns(layout);
            assert_eq!(columns.len(), column_count, "{layout:?}");
            // Reading down each column in turn is the numbered order, which is also
            // the order Tab visits the controls in.
            let flat: Vec<_> = columns.iter().flatten().copied().collect();
            assert_eq!(flat, Section::ALL, "{layout:?}");
        }
        let wide = section_columns(Wide);
        assert_eq!(
            wide,
            vec![
                vec![
                    Section::Cua,
                    Section::Codex,
                    Section::Appearance,
                    Section::Files
                ],
                vec![
                    Section::Agents,
                    Section::Windows,
                    Section::StatusBar,
                    Section::Remote,
                    Section::Orca
                ],
            ]
        );
    }

    #[test]
    fn sections_are_numbered_in_order_and_describe_themselves_in_one_line() {
        for (index, section) in Section::ALL.iter().enumerate() {
            assert_eq!(section.number(), format!("{:02}", index + 1));
            assert!(!section.title().is_empty());
            // Menlo at 10 px is about 6 px per character.
            let width = section.description().chars().count() as f32 * 6.1;
            assert!(width <= DESCRIPTION_MAX_WIDTH, "{:?} wraps", section);
        }
    }

    #[test]
    fn theme_choices_stay_in_one_column_on_narrow_panels_and_grid_when_there_is_room() {
        for width in [0.0, 300.0, 520.0, 699.9] {
            assert_eq!(theme_columns(layout_for(width), width), 1, "{width}");
        }
        assert_eq!(width_class(700.0), (SettingsLayout::Medium, 2));
        assert_eq!(width_class(900.0), (SettingsLayout::Medium, 3));
        assert_eq!(width_class(1200.0), (SettingsLayout::Wide, 2));
        assert_eq!(width_class(1800.0), (SettingsLayout::Wide, 2));
        // Wherever the grid is used, no cell is narrower than its minimum, and no
        // section card is ever wider than its pane.
        let mut width = 0.0;
        while width <= 3000.0 {
            let layout = layout_for(width);
            let inner = card_inner_width(layout, width);
            assert!(inner <= width, "{width}");
            let columns = theme_columns(layout, width);
            assert!((1..=3).contains(&columns), "{width}");
            if layout != SettingsLayout::Narrow {
                let cell = (inner - ROW_GAP * (columns - 1) as f32) / columns as f32;
                assert!(cell >= THEME_CELL_MIN_WIDTH, "{width}: {cell}");
            }
            width += 3.0;
        }
    }

    #[test]
    fn repainting_is_needed_only_when_the_arrangement_changes() {
        assert_eq!(width_class(1250.0), width_class(1900.0));
        assert_eq!(width_class(800.0), width_class(1150.0));
        assert_ne!(width_class(699.0), width_class(700.0));
        assert_ne!(width_class(1199.0), width_class(1200.0));
        // The grid gains a third column inside the medium class.
        assert_ne!(width_class(720.0), width_class(900.0));
    }
}
