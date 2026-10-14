//! Saved application preferences, shared by every RiWork window and process.

use std::{cell::Cell, collections::BTreeMap, path::PathBuf, rc::Rc};

use gpui::{
    AnyElement, App, Bounds, Context, Div, EventEmitter, FocusHandle, FontWeight, Global,
    IntoElement, KeyDownEvent, Pixels, Render, Window, canvas, div, prelude::*, px, rgb,
};
#[cfg(test)]
use serde_json::Value;
#[cfg(test)]
use std::fs;
#[cfg(test)]
use uuid::Uuid;

use crate::{
    behavior_controls as behavior,
    chat::model::Provider,
    codex_accounts::{self, AccountsSnapshot},
    controls,
    cua::{CuaManager, CuaSkills, CuaStatus, SkillLinkState},
    orca_import::{ImportManager, ImportPreview, ImportReceipt},
    project_sort::ProjectOrder,
    remote_service::{self, RemoteState},
    remote_tree::Link,
    status_bar::StatusBarSettings,
    theme::{Appearance, Palette, ThemeChoice, palette},
    ui_text::{self, InterfaceFont, SizeChange, TextPoints},
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

pub use riwork_core::settings::{
    OrchestratorMode, OrchestratorRuns, TabCloseBehavior, agent_terminals_unrestricted,
    claude_chat_ignores_api_key, orchestrator_runs,
};

/// The window's settings global. Serialization and persistence stay in core.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Settings(pub riwork_core::settings::Settings);

impl Settings {
    pub fn chat_display_for(&self, chat_id: Option<&str>) -> crate::chat_view::DisplayMode {
        self.0.chat_display_for(chat_id)
    }

    pub fn terminal_colors_forced(&self) -> bool {
        self.0.terminal_colors_forced()
    }
}

impl std::ops::Deref for Settings {
    type Target = riwork_core::settings::Settings;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for Settings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl From<riwork_core::settings::Settings> for Settings {
    fn from(settings: riwork_core::settings::Settings) -> Self {
        Self(settings)
    }
}
impl Global for Settings {}

/// Keeps GUI callbacks on the window's wrapper while core owns locking and writes.
#[derive(Clone)]
pub struct SettingsStore(riwork_core::settings::SettingsStore);
impl SettingsStore {
    pub fn open_default() -> Result<Self, String> {
        riwork_core::settings::SettingsStore::open_default().map(Self)
    }
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, String> {
        riwork_core::settings::SettingsStore::open(dir).map(Self)
    }
    pub fn load(&self) -> Result<Settings, String> {
        self.0.load().map(Settings)
    }
    pub fn update(&self, change: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
        self.0
            .update(|data| {
                let mut settings = Settings(data.clone());
                change(&mut settings);
                *data = settings.0;
            })
            .map(Settings)
    }
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
    fn set(self, settings: &mut Settings, checked: bool) {
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
        *value = checked;
    }
}

fn provider_label(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
        Provider::Muse => "Muse",
    }
}

/// The Text size row's MATCH TERMINAL control: interface text follows Ghostty's
/// `font-size`, or goes back to the saved size.
fn toggle_text_match(settings: &mut Settings) {
    settings.ui_text_matches_terminal = !settings.ui_text_matches_terminal;
}

pub struct SettingsPanel {
    store: SettingsStore,
    cua_focus: FocusHandle,
    cua_check_focus: FocusHandle,
    account_refresh_focus: FocusHandle,
    account_focus: BTreeMap<String, FocusHandle>,
    theme_focus: Vec<FocusHandle>,
    close_focus: FocusHandle,
    terminal_focus: FocusHandle,
    font_focus: FocusHandle,
    tab_icons_focus: FocusHandle,
    preview_focus: FocusHandle,
    inline_focus: FocusHandle,
    unrestricted_focus: FocusHandle,
    mic_focus: FocusHandle,
    api_key_focus: FocusHandle,
    orchestrator_mode_focus: FocusHandle,
    orchestrator_provider_focus: FocusHandle,
    size_focus: FocusHandle,
    text_size_focus: FocusHandle,
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
            Self::Cua => "Computer use",
            Self::Codex => "Codex accounts",
            Self::Appearance => "Appearance",
            Self::Files => "Files",
            Self::Agents => "Agent sessions",
            Self::Windows => "Windows",
            Self::StatusBar => "Status bar",
            Self::Remote => "Remote",
            Self::Orca => "Import from Orca",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Cua => "Let RiWork agents operate desktop apps through Cua.ai.",
            Self::Codex => "The account new Codex sessions start with.",
            Self::Appearance => "Choose a theme and text size, or sync with Ghostty.",
            Self::Files => "How the Files tree and its Preview pane open together.",
            Self::Agents => "How agent sessions draw, how orchestrators run, and dictation.",
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
    let fit = (card_inner_width(layout, width) + ROW_GAP)
        / (ui_text::space_f32(THEME_CELL_MIN_WIDTH) + ROW_GAP);
    (fit as usize).clamp(2, 3)
}

/// Everything that changes the arrangement with the panel's width. The panel
/// repaints when this changes, not on every pixel of a resize.
fn width_class(width: f32) -> (SettingsLayout, usize) {
    let layout = layout_for(width);
    (layout, theme_columns(layout, width))
}

/// The narrowest a row's title and description get beside its controls.
const ROW_TEXT_MIN_WIDTH: f32 = 220.0;

/// A row's title and description. In a row that is `flex_wrap`, it takes the room
/// beside the controls, and once that is less than `ROW_TEXT_MIN_WIDTH` the
/// controls move onto a line of their own below it instead of squeezing it.
fn row_text() -> Div {
    div()
        .flex_grow(1.0)
        .flex_shrink(1.0)
        .flex_basis(ui_text::space(ROW_TEXT_MIN_WIDTH))
        .min_w_0()
        .flex()
        .flex_col()
        .gap(ui_text::space(4.0))
}

fn row_list() -> Div {
    div().flex().flex_col().gap(ui_text::space(ROW_GAP))
}

/// Button rows sit at the end of their card once there is room for that.
fn action_bar(layout: SettingsLayout) -> Div {
    div()
        .flex()
        .flex_wrap()
        .gap(ui_text::space(7.0))
        .when(layout != SettingsLayout::Narrow, |bar| bar.justify_end())
}

fn section_card(
    section: Section,
    layout: SettingsLayout,
    colors: Palette,
    body: AnyElement,
) -> AnyElement {
    if ui_text::is_native() {
        return native_section_card(section, colors, body);
    }
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .gap(ui_text::space(12.0))
        .p(px(layout.card_padding()))
        .bg(rgb(colors.panel))
        .border_1()
        .border_color(rgb(colors.divider))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(ui_text::space(4.0))
                .pb(ui_text::space(10.0))
                .border_b_1()
                .border_color(rgb(colors.divider))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(10.0))
                        .text_size(ui_text::text(11.0))
                        .child(
                            div()
                                .text_color(rgb(colors.magenta))
                                .child(section.number()),
                        )
                        .child(
                            div()
                                .text_color(rgb(colors.cyan))
                                .child(ui_text::cased(section.title())),
                        ),
                )
                .child(
                    div()
                        .max_w(ui_text::space(DESCRIPTION_MAX_WIDTH))
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .child(section.description()),
                ),
        )
        .child(body)
        .into_any_element()
}

/// Native's section: a group on the page under a list's section heading, with no number.
fn native_section_card(section: Section, colors: Palette, body: AnyElement) -> AnyElement {
    controls::card(div(), colors)
        .flex()
        .flex_col()
        .min_w_0()
        .gap(ui_text::space(12.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(ui_text::space(2.0))
                // Headed like a navigation list's section.
                .child(
                    div()
                        .text_size(ui_text::text(controls::META_TEXT))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(colors.muted))
                        .child(section.title()),
                )
                .child(
                    div()
                        .max_w(ui_text::space(DESCRIPTION_MAX_WIDTH))
                        .text_size(ui_text::text(controls::ROW_DETAIL_TEXT))
                        .text_color(rgb(colors.muted))
                        .child(section.description()),
                ),
        )
        .child(body)
        .into_any_element()
}

/// The line box of a theme choice's title under Native, which its radio is centered on.
const THEME_TITLE_LINE: f32 = 16.0;

/// Native's radio mark: a ring, filled with the primary color when chosen.
fn radio(mark: Div, selected: bool, colors: Palette) -> Div {
    mark.size(ui_text::space(12.0))
        .mt_0()
        .rounded_full()
        .border_color(rgb(if selected {
            colors.cyan
        } else {
            colors.divider
        }))
        .when(selected, |mark| {
            mark.border(ui_text::space(4.0)).bg(rgb(colors.bg))
        })
}

/// Native's kind of a Settings button that is either a group's lead action or not.
fn button_kind(primary: bool, disabled: bool) -> controls::Button {
    if disabled {
        controls::Button::Disabled
    } else if primary {
        controls::Button::Primary
    } else {
        controls::Button::Secondary
    }
}

fn status_chip(label: impl Into<gpui::SharedString>, color: u32, colors: Palette) -> AnyElement {
    let label = label.into();
    if ui_text::is_native() {
        return controls::chip(label, color == colors.gold, colors);
    }
    div()
        .flex_none()
        .px(ui_text::space(7.0))
        .py(ui_text::space(3.0))
        .border_1()
        .border_color(rgb(color))
        .bg(rgb(colors.panel_active))
        .text_size(ui_text::text(9.0))
        .text_color(rgb(color))
        .child(ui_text::cased(label))
        .into_any_element()
}

/// The driver can be ready while a CLI still lacks the skill, which only setup
/// links; setup is then the next step.
fn skill_setup_pending(status: Option<&CuaStatus>) -> bool {
    status.is_some_and(|status| status.ready && status.skills.setup_needed())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SkillTone {
    Linked,
    Attention,
    /// Nothing to do: the CLI has no home here, or links are not made.
    Idle,
}

/// One chip per skill location. A link says a session can find the skill when
/// it starts, not that a running session has it.
fn skill_chips(skills: &CuaSkills) -> Vec<(String, SkillTone)> {
    skills
        .targets
        .iter()
        .map(|target| {
            let tone = match target.state {
                SkillLinkState::Linked => SkillTone::Linked,
                SkillLinkState::HomeAbsent | SkillLinkState::Unsupported => SkillTone::Idle,
                _ => SkillTone::Attention,
            };
            (format!("{} {}", target.name, target.state.label()), tone)
        })
        .collect()
}

fn skill_block(skills: &CuaSkills, colors: Palette) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(ui_text::space(6.0))
        .child(
            div()
                .text_size(ui_text::text(12.0))
                .child("cua-driver skill"),
        )
        .child(div().flex().flex_wrap().gap(ui_text::space(6.0)).children(
            skill_chips(skills).into_iter().map(|(label, tone)| {
                let color = match tone {
                    SkillTone::Linked => colors.cyan,
                    SkillTone::Attention => colors.gold,
                    SkillTone::Idle => colors.muted,
                };
                status_chip(label, color, colors)
            }),
        ))
        .child(
            div()
                .max_w(ui_text::space(DESCRIPTION_MAX_WIDTH))
                .text_size(ui_text::text(11.0))
                .text_color(rgb(colors.muted))
                .child(skills.message.clone()),
        )
        .into_any_element()
}

fn import_counts(projects: usize, folders: usize, worktrees: usize, colors: Palette) -> AnyElement {
    let mut counts = vec![(projects, "projects")];
    if folders > 0 {
        counts.push((folders, "folders"));
    }
    counts.push((worktrees, "worktrees"));
    div()
        .flex()
        .flex_wrap()
        .gap(ui_text::space(7.0))
        .children(counts.into_iter().map(|(count, label)| {
            div()
                .px(ui_text::space(8.0))
                .py(ui_text::space(5.0))
                .border_1()
                .border_color(rgb(colors.divider))
                .bg(rgb(colors.panel_active))
                .text_size(ui_text::text(10.0))
                .text_color(rgb(colors.cyan))
                .map(|chip| {
                    controls::native(chip, |chip| {
                        chip.rounded_full().border_0().text_color(rgb(colors.text))
                    })
                })
                .child(ui_text::cased(format!("{count} {label}")))
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
            Self::Import => Some("Import now"),
            Self::Finish => Some("Finish import"),
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
        // The Text size row describes Ghostty's size while it matches it.
        cx.observe_global::<ui_text::TerminalFontSize>(|_, cx| cx.notify())
            .detach();
        // RemoteState also changes when polling is merely scheduled. Only the
        // host rows and their error affect this panel; do not redraw for a tick.
        let mut remote_rows = remote_service::hosts(cx);
        let mut remote_error = cx
            .global::<RemoteState>()
            .tree()
            .hosts_error()
            .map(str::to_owned);
        cx.observe_global::<RemoteState>(move |_, cx| {
            let rows = remote_service::hosts(cx);
            let error = cx
                .global::<RemoteState>()
                .tree()
                .hosts_error()
                .map(str::to_owned);
            if rows != remote_rows || error != remote_error {
                remote_rows = rows;
                remote_error = error;
                cx.notify();
            }
        })
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
            close_focus: cx.focus_handle(),
            theme_focus: ThemeChoice::ALL.iter().map(|_| cx.focus_handle()).collect(),
            terminal_focus: cx.focus_handle(),
            font_focus: cx.focus_handle(),
            tab_icons_focus: cx.focus_handle(),
            preview_focus: cx.focus_handle(),
            inline_focus: cx.focus_handle(),
            unrestricted_focus: cx.focus_handle(),
            mic_focus: cx.focus_handle(),
            api_key_focus: cx.focus_handle(),
            orchestrator_mode_focus: cx.focus_handle(),
            orchestrator_provider_focus: cx.focus_handle(),
            size_focus: cx.focus_handle(),
            text_size_focus: cx.focus_handle(),
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
        if matches!(settings.theme, ThemeChoice::Ghostty | ThemeChoice::Native) {
            handles.push(self.terminal_focus.clone());
        }
        if settings.theme == ThemeChoice::Native {
            handles.push(self.font_focus.clone());
        }
        handles.push(self.tab_icons_focus.clone());
        handles.push(self.text_size_focus.clone());
        handles.push(self.preview_focus.clone());
        handles.push(self.inline_focus.clone());
        handles.push(self.unrestricted_focus.clone());
        handles.push(self.mic_focus.clone());
        handles.push(self.api_key_focus.clone());
        handles.push(self.orchestrator_mode_focus.clone());
        if settings.orchestrator_mode == OrchestratorMode::Chat {
            handles.push(self.orchestrator_provider_focus.clone());
        }
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
                        let choose_id = id.clone();
                        let active = if account.is_system_default {
                            selected.is_none()
                        } else {
                            selected.as_deref() == Some(&account.id)
                        };
                        let available = account.available;
                        let focus = self.account_focus.get(&id).cloned();
                        behavior::radio_content(
                            format!("codex-account-{id}"),
                            account.label.clone(),
                            row_text()
                                .gap(ui_text::space(3.0))
                                .child(
                                    div()
                                        .text_size(ui_text::text(12.0))
                                        .text_color(rgb(colors.text))
                                        .text_ellipsis()
                                        .child(account.label.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(ui_text::text(10.0))
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
                            active,
                        )
                        .disabled(!available)
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(ui_text::space(10.0))
                        .px(ui_text::space(ROW_PAD_X))
                        .py(ui_text::space(ROW_PAD_Y))
                        .border_1()
                        .border_color(rgb(if active { colors.cyan } else { colors.divider }))
                        .bg(rgb(if active {
                            colors.panel_active
                        } else {
                            colors.panel
                        }))
                        .when_some(focus, |row, focus| row.track_focus(&focus))
                        .focus_visible(|style| style.border_color(rgb(colors.cyan)))
                        .map(|row| controls::native(row, |row| controls::row(row, active, colors)))
                        .when(available, |row| {
                            row.hover(move |style| {
                                controls::hovered(
                                    style,
                                    controls::row_hover(active, colors),
                                    |style| style.bg(rgb(colors.panel_active)),
                                )
                            })
                        })
                        .child(status_chip(
                            if !available {
                                "Unavailable"
                            } else if active {
                                "Selected"
                            } else {
                                "Use account"
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
                        .on_change({
                            let listener = cx.listener(move |view, _, _, cx| {
                                if available {
                                    view.select_codex_account(&choose_id, cx);
                                }
                            });
                            move |_, event, window, cx| listener(event, window, cx)
                        })
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
                    .gap(ui_text::space(16.0))
                    .when(narrow, |head| head.flex_col())
                    .child(
                        div()
                            .max_w(ui_text::space(DESCRIPTION_MAX_WIDTH))
                            .text_size(ui_text::text(10.0))
                            .text_color(rgb(colors.muted))
                            .when(narrow, |note| note.w_full())
                            .when(!narrow, |note| note.flex_1().min_w_0())
                            .child("Choose the account for new Codex sessions. Running sessions keep their account. A project can override this in Project Settings."),
                    )
                    .child(
                        behavior::button_content("refresh-codex-accounts", "Refresh accounts", ui_text::cased(if state.pending { "Checking accounts…" } else { "Refresh accounts" })).disabled(state.pending).track_focus(&self.account_refresh_focus)
                            .flex_none().px(ui_text::space(10.0)).py(ui_text::space(6.0)).self_start()
                            .border_1().border_color(rgb(colors.divider))
                            .text_size(ui_text::text(10.0)).text_color(rgb(colors.cyan))
                            .hover(move |style| controls::hovered(style, controls::Button::Secondary.hover(colors), |style| style.bg(rgb(colors.panel_active))))
                            .focus_visible(|style| style.bg(rgb(colors.panel_active)).border_color(rgb(colors.cyan)))
                            .map(|button| controls::native(button, |button| controls::button(button, controls::Button::Secondary, colors)))
                            .on_click(cx.listener(|_, _, _, cx| refresh_codex_accounts(cx))),
                    ),
            )
            .child(gpui_kit::base::RadioGroup::new("settings-codex-accounts").aria_label("Default Codex account").axis(gpui::Axis::Vertical).flex().flex_col().gap(ui_text::space(ROW_GAP)).children(rows))
            .children(selected_missing.then(|| div().text_size(ui_text::text(10.0)).text_color(rgb(colors.gold))
                .child("Your selected account is unavailable. Refresh accounts or choose another before starting Codex.")))
            .children(state.snapshot.as_ref().and_then(|snapshot| snapshot.error.as_ref()).map(|error| {
                div().text_size(ui_text::text(10.0)).text_color(rgb(colors.gold)).child(error.clone())
            }))
            .children(state.snapshot.as_ref().filter(|snapshot| snapshot.from_cache).map(|_| {
                div().text_size(ui_text::text(10.0)).text_color(rgb(colors.muted)).child("Showing saved accounts while Orca is unavailable.")
            }))
            .into_any_element()
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.platform || event.keystroke.modifiers.control {
            return;
        }
        let _settings = cx.global::<Settings>().clone();
        let theme_index = self
            .theme_focus
            .iter()
            .position(|focus| focus.is_focused(window));
        match event.keystroke.key.as_str() {
            "tab" => {
                if event.keystroke.modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
            }
            "m" if self.text_size_focus.contains_focused(window, cx) => {
                self.change(toggle_text_match, cx);
            }
            "up" | "left" | "down" | "right" | "-" | "=" | "+" | "0"
                if self.text_size_focus.contains_focused(window, cx) =>
            {
                let change = match event.keystroke.key.as_str() {
                    "up" | "right" | "=" | "+" => SizeChange::Bigger,
                    "down" | "left" | "-" => SizeChange::Smaller,
                    _ => SizeChange::Reset,
                };
                self.change_text_size(change, cx);
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
        behavior::button_content(
            if import {
                "orca-import-confirm"
            } else {
                "orca-import-preview"
            },
            label,
            ui_text::cased(label),
        )
        .disabled(disabled)
        .track_focus(if import {
            &self.orca_import_focus
        } else {
            &self.orca_preview_focus
        })
        .px(ui_text::space(10.0))
        .py(ui_text::space(6.0))
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
        .text_size(ui_text::text(10.0))
        .text_color(rgb(if disabled {
            colors.muted
        } else if import {
            colors.cyan
        } else {
            colors.text
        }))
        .hover(move |style| {
            controls::hovered(
                style,
                button_kind(import, disabled).hover(colors),
                |style| style.bg(rgb(colors.panel_active)),
            )
        })
        .focus_visible(|style| style.border_color(rgb(colors.magenta)))
        .map(|button| {
            controls::native(button, |button| {
                controls::button(button, button_kind(import, disabled), colors)
            })
        })
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
            .gap(ui_text::space(9.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(10.0))
                    .child(div().flex_1().text_size(ui_text::text(13.0)).child("Orca"))
                    .child(status_chip(
                        if pending {
                            "Working"
                        } else if receipt.is_some() {
                            "Imported"
                        } else {
                            "One-time import"
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
                    .max_w(ui_text::space(DESCRIPTION_MAX_WIDTH))
                    .text_size(ui_text::text(11.0))
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
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    .child(format!("ORCA CLI · {}", source.display()))
            }))
            .children(counts)
            .children(offer.filter(|_| receipt.is_none()).map(|offer| {
                div()
                    .max_w(ui_text::space(DESCRIPTION_MAX_WIDTH))
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    .child(offer.hint())
            }))
            .children(preview.filter(|preview| !preview.warnings.is_empty()).map(|preview| {
                div()
                    .id("orca-import-warnings")
                    .max_h(ui_text::space(120.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(ui_text::space(5.0))
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.gold))
                    .children(preview.warnings.iter().cloned().map(|warning| div().child(warning)))
            }))
            .children(self.orca_pending.map(|message| {
                div()
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.cyan))
                    .child(message)
            }))
            .children(self.orca_error.as_ref().map(|error| {
                div()
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.gold))
                    .child(error.clone())
            }))
            .child(
                action_bar(layout)
                    .child(self.orca_button(
                        if receipt.is_some() {
                            "Check import"
                        } else {
                            "Preview import"
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
        behavior::button_content(id, label, ui_text::cased(label))
            .track_focus(focus)
            .px(ui_text::space(10.0))
            .py(ui_text::space(6.0))
            .border_1()
            .border_color(rgb(accent))
            .bg(rgb(colors.panel))
            .text_size(ui_text::text(10.0))
            .text_color(rgb(accent))
            .hover(move |style| {
                controls::hovered(style, controls::Button::Secondary.hover(colors), |style| {
                    style.bg(rgb(colors.panel_active))
                })
            })
            .focus_visible(|style| style.border_color(rgb(colors.magenta)))
            .map(|button| {
                // The signal color marks a destructive confirmation; it stays on the text.
                controls::native(button, |button| {
                    let button = controls::button(button, controls::Button::Secondary, colors);
                    if accent == colors.gold {
                        button.text_color(rgb(colors.gold))
                    } else {
                        button
                    }
                })
            })
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
                    .flex_wrap()
                    .items_center()
                    .gap(ui_text::space(10.0))
                    .px(ui_text::space(ROW_PAD_X))
                    .py(ui_text::space(ROW_PAD_Y))
                    .border_1()
                    .border_color(rgb(colors.divider))
                    .bg(rgb(colors.panel_active))
                    .map(|row| controls::native(row, |row| controls::row(row, true, colors)))
                    .child(div().flex_none().text_color(rgb(dot)).child(
                        if *link == Some(Link::Offline) {
                            "○"
                        } else {
                            "●"
                        },
                    ))
                    .child(
                        row_text()
                            .gap(ui_text::space(2.0))
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_size(ui_text::text(12.0))
                                    .child(host.label.clone()),
                            )
                            .child(
                                div()
                                    .text_size(ui_text::text(10.0))
                                    .text_color(rgb(colors.muted))
                                    .child(link.map_or("Connecting…", Link::text)),
                            ),
                    )
                    .child(self.remote_button(
                        format!("remote-remove-{}", host.id),
                        if confirming {
                            "Confirm remove"
                        } else {
                            "Remove"
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
            .gap(ui_text::space(9.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(10.0))
                    .child(div().flex_1().text_size(ui_text::text(13.0)).child("Other Macs"))
                    .child(status_chip(
                        if self.remote_pending.is_some() {
                            "Working"
                        } else {
                            "Encrypted"
                        },
                        colors.magenta,
                        colors,
                    )),
            )
            .child(
                div()
                    .max_w(ui_text::space(DESCRIPTION_MAX_WIDTH))
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.muted))
                    .child("Add a Mac to open its projects and shells in this window. A paired Mac has the same control over the other as a paired phone: it can type into any terminal."),
            )
            .child(row_list().children(rows))
            .children(empty.then(|| {
                div()
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    .child("No other Mac is paired yet.")
            }))
            .children(error.map(|error| {
                div()
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.gold))
                    .child(error)
            }))
            .children(self.remote_pending.map(|message| {
                div()
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.cyan))
                    .child(message)
            }))
            .children(self.remote_error.as_ref().map(|error| {
                div()
                    .text_size(ui_text::text(11.0))
                    .text_color(rgb(colors.gold))
                    .child(error.clone())
            }))
            .child(
                action_bar(layout)
                    .child(self.remote_button(
                        "remote-pair-mac".to_owned(),
                        "Pair another Mac",
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
                        "Add host",
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
            status if skill_setup_pending(status) => CuaAction::Install,
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
        behavior::button_content(
            if primary {
                "cua-setup-primary"
            } else {
                "cua-check"
            },
            label,
            ui_text::cased(label),
        )
        .disabled(disabled)
        .track_focus(if primary {
            &self.cua_focus
        } else {
            &self.cua_check_focus
        })
        .px(ui_text::space(10.0))
        .py(ui_text::space(6.0))
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
        .text_size(ui_text::text(10.0))
        .text_color(rgb(if disabled {
            colors.muted
        } else if primary {
            colors.cyan
        } else {
            colors.text
        }))
        .hover(move |style| {
            controls::hovered(
                style,
                button_kind(primary, disabled).hover(colors),
                |style| style.bg(rgb(colors.panel_active)),
            )
        })
        .focus_visible(|style| style.border_color(rgb(colors.magenta)))
        .map(|button| {
            controls::native(button, |button| {
                controls::button(button, button_kind(primary, disabled), colors)
            })
        })
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
        let link_skill = skill_setup_pending(status);
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
                .gap(ui_text::space(6.0))
                .child(status_chip(
                    if status.accessibility {
                        "Accessibility ready"
                    } else {
                        "Accessibility needed"
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
                        "Recording ready"
                    } else {
                        "Recording needed"
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
                        "Capture verified"
                    } else {
                        "Capture unverified"
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
            .gap(ui_text::space(9.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(10.0))
                    .child(div().flex_1().text_size(ui_text::text(13.0)).child("Cua.ai"))
                    .child(status_chip(
                        if state.pending.is_some() { "Working" } else if ready { "Driver ready" } else { "Setup needed" },
                        if ready { colors.cyan } else { colors.gold },
                        colors,
                    )),
            )
            .child(div().max_w(ui_text::space(DESCRIPTION_MAX_WIDTH)).text_size(ui_text::text(11.0)).text_color(rgb(if state.error.is_some() { colors.gold } else { colors.muted })).child(message))
            .children(access)
            .children((installed && !ready && !verify_capture).then(|| div().max_w(ui_text::space(DESCRIPTION_MAX_WIDTH)).text_size(ui_text::text(11.0)).text_color(rgb(colors.muted))
                .child("Enable CuaDriver in macOS Accessibility and Screen Recording to connect all agents.")))
            .child(div().max_w(ui_text::space(DESCRIPTION_MAX_WIDTH)).text_size(ui_text::text(10.0)).text_color(rgb(colors.muted)).child("New agent sessions connect automatically. Restart existing sessions to connect them."))
            .children(status.map(|status| skill_block(&status.skills, colors)))
            .child(action_bar(layout)
                .child(self.cua_button(if link_skill { "Link Cua skill" } else if ready { "Check Cua" } else if repair { "Repair Cua" } else if verify_capture { "Verify screen capture" } else if installed { "Grant macOS access" } else { "Set up Cua" }, true, state.pending.is_some(), cx))
                .child(self.cua_button("Check again", false, state.pending.is_some(), cx)))
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
        behavior::radio_content(
            ("theme-choice", index),
            theme.label(),
            {
                let mark = div()
                    .flex_none()
                    .mt(ui_text::space(4.0))
                    .size(ui_text::space(8.0))
                    .border_1()
                    .border_color(rgb(if selected { colors.cyan } else { colors.muted }))
                    .when(selected, |style| style.bg(rgb(colors.cyan)));
                if ui_text::is_native() {
                    // Centered on the title's line box, which the title is set in exactly.
                    controls::on_first_line(radio(mark, selected, colors), THEME_TITLE_LINE)
                        .into_any_element()
                } else {
                    mark.into_any_element()
                }
            },
            selected,
        )
        .track_focus(&self.theme_focus[index])
        .flex_1()
        .min_w_0()
        .flex()
        .items_start()
        .gap(ui_text::space(12.0))
        .px(ui_text::space(ROW_PAD_X))
        .py(ui_text::space(ROW_PAD_Y))
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
        .hover(move |style| {
            controls::hovered(style, controls::row_hover(selected, colors), |style| {
                style.bg(rgb(colors.panel_active))
            })
        })
        .focus_visible(|style| style.border_color(rgb(colors.magenta)))
        .map(|row| controls::native(row, |row| controls::row(row, selected, colors)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(ui_text::space(3.0))
                .child(
                    // The chip shares the title's line so a grid cell keeps its width for the text.
                    div()
                        .flex()
                        .items_center()
                        .gap(ui_text::space(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(ui_text::text(12.0))
                                .when(ui_text::is_native(), |title| {
                                    title.line_height(ui_text::space(THEME_TITLE_LINE))
                                })
                                .text_color(rgb(colors.text))
                                .child(theme.label()),
                        )
                        // Native's filled row and radio already say which is active.
                        .children(
                            (selected && !ui_text::is_native())
                                .then(|| status_chip("Active", colors.cyan, colors)),
                        ),
                )
                .child(
                    div()
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .child(theme.description()),
                ),
        )
        .on_change({
            let listener = cx.listener(move |view, _, _, cx| {
                view.change(|settings| settings.theme = theme, cx);
            });
            move |_, event, window, cx| listener(event, window, cx)
        })
        .into_any_element()
    }

    /// The theme choices in rows of `columns` equal cells; a short last row keeps its cell widths.
    fn theme_grid(
        &self,
        columns: usize,
        selected: ThemeChoice,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
            let mut row = div().flex().gap(ui_text::space(ROW_GAP));
            for _ in 0..columns {
                let cell = div().flex_1().min_w_0().flex();
                row = row.child(match cells.next() {
                    Some(theme) => cell.child(theme),
                    None => cell,
                });
            }
            grid = grid.child(row);
        }
        gpui_kit::base::RadioGroup::new("settings-theme-choices")
            .aria_label("Theme")
            .axis(gpui::Axis::Vertical)
            .child(grid)
            .into_any_element()
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
            Toggle::AgentsUnrestricted => &self.unrestricted_focus,
            Toggle::DictationMic => &self.mic_focus,
            Toggle::ClaudeChatApiKey => &self.api_key_focus,
            Toggle::WindowSize => &self.size_focus,
        };
        behavior::switch_content(
            toggle.id(),
            title,
            row_text()
                .child(
                    div()
                        .text_size(ui_text::text(12.0))
                        .text_color(rgb(colors.text))
                        .child(title),
                )
                .child(
                    div()
                        .max_w(ui_text::space(DESCRIPTION_MAX_WIDTH))
                        .text_size(ui_text::text(10.0))
                        .text_color(rgb(colors.muted))
                        .child(description),
                ),
            enabled,
        )
        .track_focus(focus)
        .flex()
        .flex_wrap()
        .items_center()
        .gap(ui_text::space(12.0))
        .px(ui_text::space(ROW_PAD_X))
        .py(ui_text::space(ROW_PAD_Y))
        .bg(rgb(colors.panel))
        .border_1()
        .border_color(rgb(colors.divider))
        .hover(move |style| {
            controls::hovered(style, controls::row_hover(false, colors), |style| {
                style.bg(rgb(colors.panel_active))
            })
        })
        .focus_visible(|style| style.border_color(rgb(colors.cyan)))
        .map(|row| controls::native(row, |row| controls::row(row, false, colors)))
        .child(if ui_text::is_native() {
            controls::switch(enabled, colors)
        } else {
            div()
                .flex_none()
                .w(ui_text::space(42.0))
                .py(ui_text::space(4.0))
                .border_1()
                .border_color(rgb(if enabled { colors.cyan } else { colors.divider }))
                .bg(rgb(colors.panel_active))
                .text_color(rgb(if enabled { colors.cyan } else { colors.muted }))
                .text_size(ui_text::text(10.0))
                .text_center()
                .child(if enabled { "ON" } else { "OFF" })
                .into_any_element()
        })
        .on_change({
            let listener = cx.listener(move |view, checked: &bool, _, cx| {
                view.change(|settings| toggle.set(settings, *checked), cx);
            });
            move |checked, _, window, cx| listener(&checked, window, cx)
        })
        .into_any_element()
    }

    fn agents_section(&self, settings: &Settings, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let chat = settings.orchestrator_mode == OrchestratorMode::Chat;
        row_list()
            .child(self.toggle_row(
                Toggle::AgentInline,
                "Keep agent transcripts in scrollback (inline mode)",
                "New Codex, Grok, and Claude sessions draw on the terminal's main screen, so the whole conversation stays in the terminal's scrollback. You can scroll it locally, and the iOS app can download and scroll it without sending keys to the agent. Off runs them full screen. A session that is already running keeps its mode until it restarts.",
                settings.agent_inline_mode,
                cx,
            ))
            .child(self.toggle_row(
                Toggle::AgentsUnrestricted,
                "Agent terminals run unrestricted",
                "Codex, Claude, Grok and Muse from the New tab menu, ⌘⇧C, ⌘⇧L, ⌘⇧G and ⌘⇧U, and from the iOS app start without approval prompts: Codex with --dangerously-bypass-approvals-and-sandbox, Claude with --dangerously-skip-permissions, Grok with --always-approve, Muse with --yolo. Off, they ask before running commands and editing files. A session that is already running keeps its mode.",
                settings.agent_terminals_unrestricted,
                cx,
            ))
            .child(self.toggle_row(
                Toggle::DictationMic,
                "Show microphone buttons for dictation",
                "Covers the mic in chat message boxes on this Mac (or ⌃⌥D) and the mic buttons on a paired iPhone, in its chats and terminals. Speech is recognized on the device you speak to and stays there. Off hides them all.",
                settings.dictation_mic,
                cx,
            ))
            .child(self.toggle_row(
                Toggle::ClaudeChatApiKey,
                "Claude chats ignore ANTHROPIC_API_KEY",
                "Starts Claude chats without ANTHROPIC_API_KEY, so a key exported in your shell cannot switch them from your Claude subscription to API billing. Off runs Claude as installed, with every sign-in method it has. Applies to new chats, and to an open chat after Stop chat. Claude terminal tabs are not affected.",
                settings.claude_chat_ignores_api_key,
                cx,
            ))
            .child(self.choice_row(
                "orchestrator-mode",
                &self.orchestrator_mode_focus,
                "Orchestrator runs as",
                "Terminal runs the global and project orchestrators as Codex in a terminal tab. Chat runs them as a native chat tab that the iOS app can follow too.",
                settings.orchestrator_mode,
                OrchestratorMode::ALL.map(|mode| {
                    (
                        mode,
                        mode.label(),
                        match mode {
                            OrchestratorMode::Terminal => "orchestrator-mode-terminal",
                            OrchestratorMode::Chat => "orchestrator-mode-chat",
                        },
                    )
                }),
                |settings, mode| settings.orchestrator_mode = mode,
                cx,
            ))
            .children(chat.then(|| {
                self.choice_row(
                    "orchestrator-provider",
                    &self.orchestrator_provider_focus,
                    "Chat provider",
                    "The agent behind a chat orchestrator. Project orchestrators never ask before they act; the global orchestrator asks before running commands and editing files.",
                    settings.orchestrator_chat_provider,
                    [Provider::Codex, Provider::Claude, Provider::Muse].map(|provider| {
                        (
                            provider,
                            provider_label(provider),
                            match provider {
                                Provider::Codex => "orchestrator-provider-codex",
                                Provider::Claude => "orchestrator-provider-claude",
                                Provider::Muse => "orchestrator-provider-muse",
                            },
                        )
                    }),
                    |settings, provider| settings.orchestrator_chat_provider = provider,
                    cx,
                )
            }))
            .child(
                div()
                    .px(ui_text::space(ROW_PAD_X))
                    .max_w(ui_text::space(DESCRIPTION_MAX_WIDTH))
                    .text_size(ui_text::text(10.0))
                    .text_color(rgb(colors.muted))
                    .child("Applies to orchestrators created from now on. An existing orchestrator keeps its mode until it is closed and created again."),
            )
            .into_any_element()
    }

    /// Base radios select a value; the owner keeps the selected tab stop stable.
    /// Arrow navigation changes selection without replacing the focus handle.
    #[allow(clippy::too_many_arguments)]
    fn choice_row<T: Copy + PartialEq + 'static, const N: usize>(
        &self,
        id: &'static str,
        focus: &FocusHandle,
        title: &'static str,
        description: &'static str,
        selected: T,
        choices: [(T, &'static str, &'static str); N],
        apply: fn(&mut Settings, T),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = palette(cx);
        let choice = |(value, label, choice_id): (T, &'static str, &'static str),
                      cx: &mut Context<Self>| {
            let active = value == selected;
            let selected_focus = focus.clone();
            behavior::radio_content(choice_id, label, label, active)
                .tab_stop(active)
                .when(active, |radio| radio.track_focus(focus))
                .flex_none()
                .px(ui_text::space(8.0))
                .py(ui_text::space(4.0))
                .border_1()
                .border_color(rgb(if active { colors.cyan } else { colors.divider }))
                .bg(rgb(colors.panel_active))
                .text_color(rgb(if active { colors.cyan } else { colors.muted }))
                .text_size(ui_text::text(10.0))
                .hover(move |style| {
                    controls::hovered(style, controls::segment_hover(active, colors), |style| {
                        style.border_color(rgb(colors.cyan))
                    })
                })
                .map(|choice| {
                    controls::native(choice, |choice| controls::segment(choice, active, colors))
                })
                .on_change({
                    let listener = cx.listener(move |view, _, window, cx| {
                        cx.stop_propagation();
                        view.change(|settings| apply(settings, value), cx);
                        selected_focus.focus(window, cx);
                    });
                    move |_, event, window, cx| listener(event, window, cx)
                })
        };
        div()
            .id(id)
            .flex()
            .flex_wrap()
            .items_center()
            .gap(ui_text::space(12.0))
            .px(ui_text::space(ROW_PAD_X))
            .py(ui_text::space(ROW_PAD_Y))
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.divider))
            .focus_visible(|style| style.border_color(rgb(colors.cyan)))
            .map(|row| controls::native(row, |row| controls::row(row, false, colors)))
            .child(
                row_text()
                    .child(
                        div()
                            .text_size(ui_text::text(12.0))
                            .text_color(rgb(colors.text))
                            .child(title),
                    )
                    .child(
                        div()
                            .max_w(ui_text::space(DESCRIPTION_MAX_WIDTH))
                            .text_size(ui_text::text(10.0))
                            .text_color(rgb(colors.muted))
                            .child(description),
                    ),
            )
            .child(
                gpui_kit::base::RadioGroup::new(format!("{id}-choices"))
                    .axis(gpui::Axis::Horizontal)
                    .aria_label(title)
                    .flex_none()
                    .flex()
                    .items_center()
                    .when(!ui_text::is_native(), |group| group.flex_wrap())
                    .gap(ui_text::space(if ui_text::is_native() { 2.0 } else { 6.0 }))
                    .when(ui_text::is_native(), |group| {
                        group
                            .p(ui_text::space(2.0))
                            .rounded_full()
                            .bg(rgb(colors.panel_active))
                    })
                    .children(choices.map(|choice_value| choice(choice_value, cx)))
                    .on_key_down(cx.listener(move |view, event: &KeyDownEvent, window, cx| {
                        if matches!(
                            event.keystroke.key.as_str(),
                            "left" | "right" | "up" | "down"
                        ) && !event.keystroke.modifiers.modified()
                        {
                            let at = choices
                                .iter()
                                .position(|(value, _, _)| *value == selected)
                                .unwrap_or(0);
                            let step = if matches!(event.keystroke.key.as_str(), "left" | "up") {
                                N - 1
                            } else {
                                1
                            };
                            let next = choices[(at + step) % N].0;
                            view.change(|settings| apply(settings, next), cx);
                            cx.stop_propagation();
                            window.prevent_default();
                        }
                    })),
            )
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
        let terminal_row = match settings.theme {
            ThemeChoice::Ghostty => Some(self.toggle_row(
                Toggle::TerminalColors,
                "Use RiWork terminal colors",
                "Keep RiWork terminal colors while following Ghostty. Off uses Ghostty colors.",
                settings.use_riwork_colors,
                cx,
            )),
            ThemeChoice::Native => Some(self.toggle_row(
                Toggle::TerminalColors,
                "Terminals match the theme",
                "Terminals take the theme's white or black background and a quiet palette. Off keeps Ghostty's own colors.",
                settings.native_terminal_colors,
                cx,
            )),
            _ => None,
        };
        let font_row = (settings.theme == ThemeChoice::Native)
            .then(|| self.font_row(settings.interface_font, cx));
        row_list()
            .child(self.theme_grid(theme_columns, settings.theme, cx))
            .children(appearance_error.map(|error| div().text_size(ui_text::text(11.0)).text_color(rgb(colors.gold)).child(error)))
            .children(terminal_row)
            .children(font_row)
            .child(self.toggle_row(
                Toggle::PanelTabIcons,
                "Icons instead of labels",
                "Show icons instead of words on the panel tabs and on toolbar buttons, such as the create buttons in Projects and the file actions in Files. Hover an icon for its name.",
                settings.panel_tab_icons,
                cx,
            ))
            .child(self.choice_row(
                "tab-close-behavior",
                &self.close_focus,
                "When closing a tab",
                "Ask offers Detach, which hides the tab and keeps the session running, or Exit, which stops it. Workers always detach. This Mac only.",
                settings.tab_close_behavior,
                [
                    (TabCloseBehavior::Ask, "Ask", "tab-close-ask"),
                    (TabCloseBehavior::Detach, "Detach", "tab-close-detach"),
                    (TabCloseBehavior::Exit, "Exit", "tab-close-exit"),
                ],
                |settings, choice| settings.tab_close_behavior = choice,
                cx,
            ))
            .child(self.text_size_row(settings, cx))
            .into_any_element()
    }

    /// Native's Interface font: Base radios with a persistent selected tab stop.
    fn font_row(&self, selected: InterfaceFont, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let choice = |font: InterfaceFont, cx: &mut Context<Self>| {
            let active = font == selected;
            let selected_focus = self.font_focus.clone();
            behavior::radio_content(
                match font {
                    InterfaceFont::System => "interface-font-system",
                    InterfaceFont::SystemMono => "interface-font-system-mono",
                },
                font.label(),
                font.label(),
                active,
            )
            .tab_stop(active)
            .when(active, |radio| radio.track_focus(&self.font_focus))
            .flex_none()
            .px(ui_text::space(8.0))
            .py(ui_text::space(4.0))
            .border_1()
            .border_color(rgb(if active { colors.cyan } else { colors.divider }))
            .bg(rgb(colors.panel_active))
            .text_color(rgb(if active { colors.cyan } else { colors.muted }))
            .text_size(ui_text::text(10.0))
            .hover(move |style| {
                controls::hovered(style, controls::segment_hover(active, colors), |style| {
                    style.border_color(rgb(colors.cyan))
                })
            })
            .map(|choice| {
                controls::native(choice, |choice| controls::segment(choice, active, colors))
            })
            .on_change({
                let listener = cx.listener(move |view, _, window, cx| {
                    cx.stop_propagation();
                    view.change(|settings| settings.interface_font = font, cx);
                    selected_focus.focus(window, cx);
                });
                move |_, event, window, cx| listener(event, window, cx)
            })
        };
        div()
            .id("interface-font")
            .flex()
            .flex_wrap()
            .items_center()
            .gap(ui_text::space(12.0))
            .px(ui_text::space(ROW_PAD_X))
            .py(ui_text::space(ROW_PAD_Y))
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.divider))
            .focus_visible(|style| style.border_color(rgb(colors.cyan)))
            .map(|row| controls::native(row, |row| controls::row(row, false, colors)))
            .child(
                row_text()
                    .child(
                        div()
                            .text_size(ui_text::text(12.0))
                            .text_color(rgb(colors.text))
                            .child("Interface font"),
                    )
                    .child(
                        div()
                            .max_w(ui_text::space(DESCRIPTION_MAX_WIDTH))
                            .text_size(ui_text::text(10.0))
                            .text_color(rgb(colors.muted))
                            .child("SF Pro throughout, or with paths, branches, shortcut keys and counts in a monospace face."),
                    ),
            )
            .child(
                gpui_kit::base::RadioGroup::new("interface-font-choices")
                    .axis(gpui::Axis::Horizontal).aria_label("Interface font")
                    .flex().flex_wrap().gap(ui_text::space(if ui_text::is_native() { 2.0 } else { 6.0 }))
                    .when(ui_text::is_native(), |group| group.p(ui_text::space(2.0)).rounded_full().bg(rgb(colors.panel_active)))
                    .children(InterfaceFont::ALL.map(|font| choice(font, cx)))
                    .on_key_down(cx.listener(move |view, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "left" | "right" | "up" | "down") && !event.keystroke.modifiers.modified() {
                            view.change(|settings| settings.interface_font = selected.other(), cx);
                            cx.stop_propagation();
                        }
                    })),
            )
            .into_any_element()
    }

    fn change_text_size(&mut self, change: SizeChange, cx: &mut Context<Self>) {
        match ui_text::save(&self.store, change, cx) {
            Ok(settings) => {
                cx.set_global(settings);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    /// RiWork's text size: − / the size / + / reset, and MATCH TERMINAL to follow
    /// Ghostty's `font-size`. The row takes focus like a toggle: arrows or − and =
    /// step, 0 resets, and M switches matching; the switch owns Enter and Space. While matching, the
    /// size is Ghostty's and dimmed, and stepping it stops matching.
    fn text_size_row(&self, settings: &Settings, cx: &mut Context<Self>) -> AnyElement {
        let colors = palette(cx);
        let shown = ui_text::current_points(cx);
        let matching = settings.ui_text_matches_terminal;
        let value_color = if matching { colors.muted } else { colors.cyan };
        let button = |id: &'static str,
                      label: &'static str,
                      change: SizeChange,
                      cx: &mut Context<Self>| {
            let limit = match change {
                SizeChange::Bigger => shown >= ui_text::MAX_POINTS,
                SizeChange::Smaller => shown <= ui_text::MIN_POINTS,
                SizeChange::Reset => shown == ui_text::DEFAULT_POINTS && !matching,
            };
            let active = !matching && !limit;
            behavior::button_content(id, label, ui_text::cased(label))
                .disabled(!active)
                .flex_none()
                .min_w(ui_text::space(26.0))
                .px(ui_text::space(6.0))
                .py(ui_text::space(4.0))
                .border_1()
                .border_color(rgb(if active { colors.cyan } else { colors.divider }))
                .bg(rgb(colors.panel_active))
                .text_color(rgb(if active { colors.cyan } else { colors.muted }))
                .text_size(ui_text::text(10.0))
                .text_center()
                .hover(move |style| {
                    controls::hovered(style, button_kind(false, !active).hover(colors), |style| {
                        style.border_color(rgb(colors.cyan))
                    })
                })
                .map(|button| {
                    controls::native(button, |button| {
                        controls::button(button, button_kind(false, !active), colors)
                            .px(ui_text::space(8.0))
                    })
                })
                .on_click(cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    view.change_text_size(change, cx);
                }))
        };
        let description = match ui_text::terminal_font_size(cx).filter(|_| matching) {
            Some(size) => {
                format!("Panels, tabs, menus and status bar. Matches Ghostty's font-size = {size}.")
            }
            None => "Panels, tabs, menus and status bar.".to_owned(),
        };
        // Each shortcut is one unbreakable item: wrapping inside the text broke
        // lines between "⌘" and "−".
        let shortcut = |key: &'static str, label: &'static str| {
            div().flex_none().whitespace_nowrap().child(
                div()
                    .flex()
                    .gap(ui_text::space(4.0))
                    .child(
                        div()
                            .font_family(ui_text::mono_family())
                            .text_color(rgb(colors.text))
                            .child(key),
                    )
                    .child(label),
            )
        };
        let shortcuts = div()
            .flex()
            .flex_wrap()
            .gap_x(ui_text::space(10.0))
            .text_size(ui_text::text(10.0))
            .text_color(rgb(colors.muted))
            .child(shortcut("⌘+", "bigger"))
            .child(shortcut("⌘−", "smaller"))
            .child(shortcut("⌘0", "reset"))
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .child("outside a terminal"),
            );
        let match_button = behavior::switch_content(
            "ui-text-match",
            "Match terminal text size",
            if ui_text::is_native() {
                div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(6.0))
                    .child("Match terminal")
                    .child(controls::switch(matching, colors))
                    .into_any_element()
            } else {
                div()
                    .child(if matching {
                        "MATCH TERMINAL · ON"
                    } else {
                        "MATCH TERMINAL · OFF"
                    })
                    .into_any_element()
            },
            matching,
        )
        .flex_none()
        .px(ui_text::space(6.0))
        .py(ui_text::space(4.0))
        .border_1()
        .border_color(rgb(if matching {
            colors.cyan
        } else {
            colors.divider
        }))
        .bg(rgb(colors.panel_active))
        .text_color(rgb(if matching { colors.cyan } else { colors.muted }))
        .text_size(ui_text::text(10.0))
        .text_center()
        .hover(move |style| {
            if ui_text::is_native() {
                style.text_color(rgb(colors.text))
            } else {
                style.border_color(rgb(colors.cyan))
            }
        })
        .map(|button| {
            // Native: a labelled switch, the same control as the toggle rows.
            controls::native(button, |button| {
                button
                    .flex()
                    .items_center()
                    .gap(ui_text::space(6.0))
                    .border_0()
                    .bg(gpui::transparent_black())
                    .text_color(rgb(colors.muted))
            })
        })
        .on_change({
            let listener = cx.listener(|view, checked: &bool, _, cx| {
                cx.stop_propagation();
                view.change(|settings| settings.ui_text_matches_terminal = *checked, cx);
            });
            move |checked, _, window, cx| listener(&checked, window, cx)
        });
        div()
            .id("ui-text-size")
            .track_focus(&self.text_size_focus)
            .flex()
            .flex_wrap()
            .items_center()
            .gap(ui_text::space(12.0))
            .px(ui_text::space(ROW_PAD_X))
            .py(ui_text::space(ROW_PAD_Y))
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.divider))
            .focus_visible(|style| style.border_color(rgb(colors.cyan)))
            .map(|row| controls::native(row, |row| controls::row(row, false, colors)))
            .child(
                row_text()
                    .child(
                        div()
                            .text_size(ui_text::text(12.0))
                            .text_color(rgb(colors.text))
                            .child("Text size"),
                    )
                    .child(
                        div()
                            .max_w(ui_text::space(DESCRIPTION_MAX_WIDTH))
                            .text_size(ui_text::text(10.0))
                            .text_color(rgb(colors.muted))
                            .child(description),
                    )
                    .child(shortcuts),
            )
            .child(
                div()
                    .flex_none()
                    .max_w_full()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(ui_text::space(4.0))
                    .child(button("ui-text-smaller", "−", SizeChange::Smaller, cx))
                    .child(
                        div()
                            .flex_none()
                            .min_w(ui_text::space(44.0))
                            .text_size(ui_text::text(10.0))
                            .text_center()
                            .text_color(rgb(value_color))
                            .child(ui_text::label(ui_text::shown_points(settings, shown))),
                    )
                    .child(button("ui-text-bigger", "+", SizeChange::Bigger, cx))
                    // RESET only when there is something to reset; Cmd+0 always works.
                    .when(!matching && shown != ui_text::DEFAULT_POINTS, |controls| {
                        controls.child(button("ui-text-reset", "Reset", SizeChange::Reset, cx))
                    })
                    .child(match_button),
            )
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
            Section::Agents => self.agents_section(settings, cx),
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
        let native = ui_text::is_native();
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
                    // Native's groups are separated by their own hairlines.
                    .when(native, |column| column.gap_0())
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
            // Native: a navigation panel's page and header, its cards on the sidebar grey.
            .when(native, |page| {
                page.flex()
                    .flex_col()
                    .bg(rgb(colors.panel))
                    .font_family(ui_text::ui_family())
                    .child(controls::panel_header(
                        "Settings",
                        Some("All projects · Saved automatically".into()),
                        [],
                        colors,
                    ))
            })
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
                    .when(native, |scroll| scroll.flex_1().min_h_0())
                    .overflow_y_scroll()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .justify_center()
                            .p(px(layout.page_padding()))
                            .when(native, |page| {
                                page.pt_0()
                                    .px(ui_text::space(controls::LIST_MARGIN))
                                    .pb(ui_text::space(controls::PANEL_INSET))
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .w_full()
                                    .min_w_0()
                                    .when_some(layout.max_width(), |page, max| page.max_w(px(max)))
                                    .gap(px(layout.gap()))
                                    .font_family(ui_text::ui_family())
                                    .text_color(rgb(colors.text))
                                    .when(native, |page| page.gap_0())
                                    .children((!native).then(|| {
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(ui_text::space(12.0))
                                            .pb(ui_text::space(14.0))
                                            .when(!native, |head| head.border_b_1().border_color(rgb(colors.cyan)))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex()
                                                    .flex_col()
                                                    .gap(ui_text::space(4.0))
                                                    .children((!native).then(|| div().text_size(ui_text::text(10.0)).text_color(rgb(colors.magenta)).child("RIWORK / PREFERENCES")))
                                                    .child(div().text_size(ui_text::text(20.0)).when(native, |title| title.font_weight(FontWeight::BOLD)).child("Settings")),
                                            )
                                            .child(status_chip("All projects", colors.magenta, colors))
                                    }))
                                    // Columns only differ in the wide layout; otherwise this is one stack of cards.
                                    .child(div().flex().items_start().gap(px(layout.gap())).children(columns))
                                    .children(self.error.as_ref().map(|error| div().text_size(ui_text::text(11.0)).text_color(rgb(colors.gold)).child(error.clone())))
                                    .child(div().pt(ui_text::space(10.0)).when(!native, |foot| foot.border_t_1().border_color(rgb(colors.divider))).when(native, |foot| foot.pt(ui_text::space(2.0)).px(ui_text::space(controls::PANEL_INSET - controls::LIST_MARGIN)).text_size(ui_text::text(9.0))).text_size(ui_text::text(10.0)).text_color(rgb(colors.muted)).child(if native { "Tab to move · Enter or Space to select · Arrows step the text size" } else { "Saved automatically · Tab to move · Enter or Space to select · Arrows step the text size" })),
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
    fn saving_chat_modes_preserves_unknown_entries_until_that_chat_changes() {
        use crate::chat_view::DisplayMode::{Normal, Verbose};
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
            .update(|settings| settings.orchestrator_chat_provider = Provider::Claude)
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
    }

    fn cua_status(ready: bool, pack: Option<bool>, states: &[SkillLinkState]) -> CuaStatus {
        use crate::cua::{SkillCli, SkillTarget};
        let clis = [
            SkillCli::Claude,
            SkillCli::Codex,
            SkillCli::Grok,
            SkillCli::Muse,
        ];
        CuaStatus {
            installed: true,
            driver_path: Some(PathBuf::from(
                "/Applications/CuaDriver.app/Contents/MacOS/cua-driver",
            )),
            version: Some("cua-driver 0.30.4".into()),
            accessibility: ready,
            screen_recording: ready,
            direct_capture_verified: ready,
            running: true,
            ready,
            message: String::new(),
            skills: CuaSkills {
                pack: pack.map(|_| PathBuf::from("/u/.cua-driver/skills/cua-driver")),
                pack_installed: pack == Some(true),
                message: String::new(),
                targets: clis
                    .into_iter()
                    .zip(states)
                    .map(|(cli, state)| SkillTarget {
                        cli,
                        name: cli.label().to_owned(),
                        path: PathBuf::from("/u/skills/cua-driver"),
                        state: *state,
                        variable: None,
                        homes: vec![],
                        detail: None,
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn computer_use_keeps_driver_readiness_apart_from_the_skill() {
        use SkillLinkState::*;
        let linked = cua_status(true, Some(true), &[Linked, Linked, HomeAbsent, Linked]);
        assert!(!skill_setup_pending(Some(&linked)));
        assert_eq!(
            skill_chips(&linked.skills),
            [
                ("Claude Code linked".to_owned(), SkillTone::Linked),
                ("Codex linked".to_owned(), SkillTone::Linked),
                ("Grok no home".to_owned(), SkillTone::Idle),
                ("Muse linked".to_owned(), SkillTone::Linked),
            ]
        );
        // A ready driver with a CLI that setup can still link offers setup.
        for states in [
            [Missing, Linked, Linked, Linked],
            [Linked, Stale, Linked, Linked],
        ] {
            assert!(skill_setup_pending(Some(&cua_status(
                true,
                Some(true),
                &states
            ))));
        }
        assert!(skill_setup_pending(Some(&cua_status(
            true,
            Some(false),
            &[PackMissing; 4]
        ))));
        // What setup cannot change, and a driver that is not ready, do not.
        let blocked = cua_status(
            true,
            Some(true),
            &[BlockedByForeignPath, Linked, Linked, Linked],
        );
        assert!(!skill_setup_pending(Some(&blocked)));
        assert_eq!(
            skill_chips(&blocked.skills)[0],
            (
                "Claude Code blocked by foreign path".to_owned(),
                SkillTone::Attention
            )
        );
        assert!(!skill_setup_pending(Some(&cua_status(
            true,
            None,
            &[PackMissing; 4]
        ))));
        assert!(!skill_setup_pending(Some(&cua_status(
            false,
            Some(true),
            &[Missing; 4]
        ))));
        assert!(!skill_setup_pending(None));
    }
}
