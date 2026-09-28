//! Saved application preferences, shared by every RiWork window and process.

use std::{
    collections::BTreeMap,
    env, fs,
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
};

use fs2::FileExt;
use gpui::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Global, IntoElement, KeyDownEvent,
    MouseButton, Render, Window, div, prelude::*, px, rgb,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    codex_accounts::{self, AccountsSnapshot},
    cua::{CuaManager, CuaStatus},
    orca_import::{ImportManager, ImportPreview, ImportReceipt},
    project_sort::ProjectOrder,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub schema_version: u32,
    pub theme: ThemeChoice,
    /// Kept for older preferences and as a terminal-only override while following Ghostty.
    pub use_riwork_colors: bool,
    pub remember_window_size: bool,
    pub project_order: ProjectOrder,
    pub selected_codex_account: Option<String>,
    pub status_bar: StatusBarSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            theme: ThemeChoice::Ghostty,
            use_riwork_colors: false,
            remember_window_size: true,
            project_order: ProjectOrder::default(),
            selected_codex_account: None,
            status_bar: StatusBarSettings::default(),
        }
    }
}

impl Global for Settings {}

#[derive(Clone)]
pub struct SettingsStore {
    dir: PathBuf,
}

impl SettingsStore {
    pub fn open_default() -> Result<Self, String> {
        let dir = match env::var_os("RIWORK_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(env::var_os("HOME").ok_or("HOME is unset; set RIWORK_HOME")?)
                .join(".local/share/riwork"),
        };
        Self::open(dir)
    }

    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, String> {
        let dir = dir.into();
        fs::create_dir_all(&dir)
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
        let mut settings = self.read()?;
        change(&mut settings);
        let path = self.dir.join("settings.json");
        let temporary = self.dir.join(format!(".settings-{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| format!("Cannot create settings: {error}"))?;
            serde_json::to_writer_pretty(&mut file, &settings)
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
        let path = self.dir.join("settings.json");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Settings::default());
            }
            Err(error) => return Err(format!("Cannot read settings: {error}")),
        };
        let settings: Settings = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Cannot parse settings: {error}"))?;
        if settings.schema_version != 1 {
            return Err(format!(
                "Unsupported settings schema {}",
                settings.schema_version
            ));
        }
        Ok(settings)
    }
}

pub enum SettingsEvent {
    OrcaImported,
}

pub struct SettingsPanel {
    store: SettingsStore,
    cua_focus: FocusHandle,
    cua_check_focus: FocusHandle,
    account_refresh_focus: FocusHandle,
    account_focus: BTreeMap<String, FocusHandle>,
    theme_focus: Vec<FocusHandle>,
    terminal_focus: FocusHandle,
    size_focus: FocusHandle,
    orca_preview_focus: FocusHandle,
    orca_import_focus: FocusHandle,
    orca_preview: Option<ImportPreview>,
    orca_receipt: Option<ImportReceipt>,
    orca_pending: Option<&'static str>,
    orca_error: Option<String>,
    orca_initialized: bool,
    error: Option<String>,
}

impl EventEmitter<SettingsEvent> for SettingsPanel {}

fn section_heading(number: &'static str, title: &'static str, colors: Palette) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .mt(px(8.0))
        .text_size(px(10.0))
        .child(div().text_color(rgb(colors.magenta)).child(number))
        .child(div().text_color(rgb(colors.cyan)).child(title))
        .child(div().flex_1().h(px(1.0)).bg(rgb(colors.divider)))
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
            size_focus: cx.focus_handle(),
            orca_preview_focus: cx.focus_handle(),
            orca_import_focus: cx.focus_handle(),
            orca_preview: None,
            orca_receipt: None,
            orca_pending: None,
            orca_error: None,
            orca_initialized: false,
            error: None,
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
        handles.push(self.size_focus.clone());
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

    fn codex_accounts_section(&self, cx: &mut Context<Self>) -> AnyElement {
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
                            .p(px(10.0))
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
        div().flex().flex_col().gap(px(8.0))
            .child(div().text_size(px(10.0)).text_color(rgb(colors.muted)).child("Choose the account for new Codex sessions. Running sessions keep their account."))
            .child(div().id("refresh-codex-accounts").track_focus(&self.account_refresh_focus)
                .flex().items_center().gap(px(8.0)).py(px(5.0)).cursor_pointer()
                .text_size(px(10.0)).text_color(rgb(colors.cyan))
                .focus_visible(|style| style.bg(rgb(colors.panel_active)))
                .child(if state.pending { "CHECKING ACCOUNTS…" } else { "REFRESH ACCOUNTS" })
                .on_mouse_down(MouseButton::Left, cx.listener(|view, _, window, cx| view.account_refresh_focus.focus(window, cx)))
                .on_click(cx.listener(|_, _, _, cx| refresh_codex_accounts(cx))))
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
                    self.change(
                        |settings| settings.use_riwork_colors = !settings.use_riwork_colors,
                        cx,
                    );
                } else if self.size_focus.is_focused(window) {
                    self.change(
                        |settings| settings.remember_window_size = !settings.remember_window_size,
                        cx,
                    );
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
                .is_some_and(|preview| preview.already_imported.is_none())
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

    fn orca_section(&self, cx: &mut Context<Self>) -> AnyElement {
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
        let empty = preview.is_some_and(|preview| {
            preview.project_count == 0 && preview.folder_count == 0 && preview.worktree_count == 0
        });
        div()
            .id("orca-import")
            .flex()
            .flex_col()
            .gap(px(9.0))
            .p(px(12.0))
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.divider))
            .border_l_2()
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
            .children((preview.is_some() && receipt.is_none()).then(|| {
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(colors.muted))
                    .child(if empty {
                        "Nothing new to add. Finish to save the one-time import receipt."
                    } else {
                        "These records will be added to RiWork when you import."
                    })
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
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(7.0))
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
                    .children((preview.is_some() && receipt.is_none()).then(|| {
                        self.orca_button(
                            if empty { "FINISH IMPORT" } else { "IMPORT NOW" },
                            true,
                            !self.can_import_orca(),
                            cx,
                        )
                    })),
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

    fn cua_section(&self, cx: &mut Context<Self>) -> AnyElement {
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
            .p(px(12.0))
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.divider))
            .border_l_2()
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
            .child(div().text_size(px(11.0)).text_color(rgb(if state.error.is_some() { colors.gold } else { colors.muted })).child(message))
            .children(access)
            .children((installed && !ready && !verify_capture).then(|| div().text_size(px(11.0)).text_color(rgb(colors.muted))
                .child("Enable CuaDriver in macOS Accessibility and Screen Recording to connect all agents.")))
            .child(div().flex().flex_wrap().gap(px(7.0))
                .child(self.cua_button(if ready { "CHECK CUA" } else if repair { "REPAIR CUA" } else if verify_capture { "VERIFY SCREEN CAPTURE" } else if installed { "GRANT MACOS ACCESS" } else { "SET UP CUA" }, true, state.pending.is_some(), cx))
                .child(self.cua_button("CHECK AGAIN", false, state.pending.is_some(), cx)))
            .child(div().text_size(px(10.0)).text_color(rgb(colors.muted)).child("New agent sessions connect automatically. Restart existing sessions to connect them."))
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
            .flex()
            .items_center()
            .gap(px(12.0))
            .px(px(12.0))
            .py(px(8.0))
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
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(colors.text))
                            .child(theme.label()),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(colors.muted))
                            .child(theme.description()),
                    ),
            )
            .children(selected.then(|| status_chip("ACTIVE", colors.cyan, colors)))
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

    fn toggle_row(
        &self,
        title: &'static str,
        description: &'static str,
        enabled: bool,
        terminal: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = palette(cx);
        let focus = if terminal {
            &self.terminal_focus
        } else {
            &self.size_focus
        };
        div()
            .id(if terminal {
                "terminal-colors"
            } else {
                "remember-window-size"
            })
            .track_focus(focus)
            .flex()
            .items_center()
            .gap(px(12.0))
            .p(px(12.0))
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
                    if terminal {
                        &view.terminal_focus
                    } else {
                        &view.size_focus
                    }
                    .focus(window, cx);
                }),
            )
            .on_click(cx.listener(move |view, _, _, cx| {
                view.change(
                    |settings| {
                        if terminal {
                            settings.use_riwork_colors = !settings.use_riwork_colors;
                        } else {
                            settings.remember_window_size = !settings.remember_window_size;
                        }
                    },
                    cx,
                );
            }))
            .into_any_element()
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx.global::<Settings>().clone();
        let colors = palette(cx);
        let appearance_error = cx.global::<Appearance>().error.clone();
        let theme_rows: Vec<_> = ThemeChoice::ALL
            .iter()
            .copied()
            .enumerate()
            .map(|(index, theme)| self.theme_row(index, theme, settings.theme == theme, cx))
            .collect();
        let terminal_row = (settings.theme == ThemeChoice::Ghostty).then(|| {
            self.toggle_row(
                "Use RiWork terminal colors",
                "Keep RiWork terminal colors while following Ghostty. Off uses Ghostty colors.",
                settings.use_riwork_colors,
                true,
                cx,
            )
        });
        div()
            .id("settings-panel")
            .key_context("RiWorkSettings")
            .on_key_down(cx.listener(Self::key_down))
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(colors.bg))
            .p(px(20.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(px(760.0))
                    .gap(px(8.0))
                    .font_family("Menlo")
                    .text_color(rgb(colors.text))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.0))
                            .pb(px(14.0))
                            .mb(px(2.0))
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
                    .child(section_heading("01", "COMPUTER USE", colors))
                    .child(self.cua_section(cx))
                    .child(section_heading("02", "CODEX ACCOUNTS", colors))
                    .child(self.codex_accounts_section(cx))
                    .child(section_heading("03", "APPEARANCE", colors))
                    .child(div().text_size(px(10.0)).text_color(rgb(colors.muted)).child("Choose a theme, or sync with Ghostty."))
                    .children(theme_rows)
                    .children(appearance_error.map(|error| div().text_size(px(11.0)).text_color(rgb(colors.gold)).child(error)))
                    .children(terminal_row)
                    .child(section_heading("04", "WINDOWS", colors))
                    .child(self.toggle_row(
                        "Remember project window size",
                        "Reopen projects at their last size. Switching projects keeps the current window size.",
                        settings.remember_window_size,
                        false,
                        cx,
                    ))
                    .child(section_heading("05", "STATUS BAR", colors))
                    .child(crate::status_bar::render_settings(&settings.status_bar, |view: &mut Self, status, _, cx| {
                        view.change(move |settings| settings.status_bar = status, cx);
                    }, cx))
                    .child(section_heading("06", "IMPORT FROM ORCA", colors))
                    .child(self.orca_section(cx))
                    .children(self.error.as_ref().map(|error| div().text_size(px(11.0)).text_color(rgb(colors.gold)).child(error.clone())))
                    .child(div().mt(px(8.0)).pt(px(10.0)).border_t_1().border_color(rgb(colors.divider)).text_size(px(10.0)).text_color(rgb(colors.muted)).child("Saved automatically · Tab to move · Enter or Space to select")),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
