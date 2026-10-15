//! The status bar's account items. The usage item reads out the quota of the current chat's
//! account, or of every known account (`UsageMode`). The Codex account item says which account
//! new Codex chats and shells of this project get, and its menu chooses it: the project's own
//! choice that Project Settings saves (`ProjectCodexAccount`). Every new Codex session of the
//! project resolves that choice from the store when it launches, the orchestrator's workers
//! included; sessions that run keep their account, and an explicit `--account` still wins.
//! Nothing here reads an account or its usage: it draws what the window already has.

use super::*;
use codex_accounts::{AccountsSnapshot, SYSTEM_DEFAULT_ID};
use status_bar::UsageMode;

const SYSTEM_LABEL: &str = "System default";

// ---- Which account ------------------------------------------------------------------------

/// A Codex account the bar can read out: its home, which keys `AccountUsage`, and its label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CodexProfile {
    pub(super) home: PathBuf,
    pub(super) label: String,
}

/// What a choice of Codex account comes to in the account list the window has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Selection {
    Account(CodexProfile),
    /// A saved account the list does not have, or has without a usable home.
    Unavailable,
    /// A saved account, while no list has been read.
    Unknown,
}

/// What `selected` names (None, or the system default's id, for the system default), resolved
/// as a launch resolves it (`codex_accounts::resolve_launch_binding`) but from the list in
/// memory, so nothing is read.
pub(super) fn resolve_selection(
    selected: Option<&str>,
    snapshot: Option<&AccountsSnapshot>,
) -> Selection {
    let selected = selected.filter(|id| *id != SYSTEM_DEFAULT_ID);
    let Some(snapshot) = snapshot else {
        return match selected {
            Some(_) => Selection::Unknown,
            None => codex_accounts::default_codex_home().map_or(Selection::Unavailable, |home| {
                Selection::Account(CodexProfile {
                    home,
                    label: SYSTEM_LABEL.to_owned(),
                })
            }),
        };
    };
    snapshot
        .accounts
        .iter()
        .find(|account| {
            account.available
                && match selected {
                    Some(id) => account.id == id,
                    None => account.is_system_default,
                }
        })
        .map_or(Selection::Unavailable, |account| {
            Selection::Account(CodexProfile {
                home: account.home.clone(),
                label: account.label.clone(),
            })
        })
}

/// The account new Codex sessions of a project get with `choice`, the app's selection standing
/// in for Inherit (`codex_accounts::resolve_project_choice_binding`).
pub(super) fn project_selection(
    choice: &ProjectCodexAccount,
    app_selected: Option<&str>,
    snapshot: Option<&AccountsSnapshot>,
) -> Selection {
    match choice {
        ProjectCodexAccount::Inherit => resolve_selection(app_selected, snapshot),
        ProjectCodexAccount::SystemDefault => resolve_selection(None, snapshot),
        ProjectCodexAccount::Saved(id) => resolve_selection(Some(id), snapshot),
    }
}

/// The accounts whose usage the window reads (`request_codex_usage`), in the list's order with
/// the system default first; before a list is read, the system default alone.
pub(super) fn known_codex_profiles(snapshot: Option<&AccountsSnapshot>) -> Vec<CodexProfile> {
    let Some(snapshot) = snapshot else {
        return codex_accounts::default_codex_home()
            .map(|home| {
                vec![CodexProfile {
                    home,
                    label: SYSTEM_LABEL.to_owned(),
                }]
            })
            .unwrap_or_default();
    };
    let mut homes = HashSet::new();
    snapshot
        .accounts
        .iter()
        .filter(|account| account.available && homes.insert(account.home.clone()))
        .map(|account| CodexProfile {
            home: account.home.clone(),
            label: account.label.clone(),
        })
        .collect()
}

/// The account a Codex shell runs under, by the home it was started with.
fn shell_selection(shell: &ShellSession, snapshot: Option<&AccountsSnapshot>) -> Selection {
    let Some(home) = shell.codex_home.clone() else {
        return Selection::Unknown;
    };
    let label = snapshot
        .and_then(|snapshot| {
            snapshot
                .accounts
                .iter()
                .find(|account| account.home == home)
        })
        .map(|account| account.label.clone())
        .or_else(|| verified_shell_email(shell, snapshot))
        .or_else(|| shell.codex_account_label.clone())
        .unwrap_or_else(|| {
            if shell.codex_account_id.is_none() {
                SYSTEM_LABEL.to_owned()
            } else {
                "Session account".to_owned()
            }
        });
    Selection::Account(CodexProfile { home, label })
}

/// What Codex itself said the account at `home` is, from its last usage read.
fn reported_account_label(
    usage: &BTreeMap<PathBuf, CodexUsageEntry>,
    home: Option<&Path>,
) -> Option<String> {
    usage.get(home?)?.codex.as_ref()?.account_label.clone()
}

// ---- The usage readouts -------------------------------------------------------------------

/// What the focused tab runs, as the usage item reads it in Current chat.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum UsageFocus {
    /// A Codex chat or shell, under this account. `number` is the account's number in the
    /// project's tab titles (`A2`), when more than one account runs there.
    Codex {
        account: Selection,
        number: Option<usize>,
    },
    ClaudeShell(String),
    GrokShell {
        id: String,
        alive: bool,
    },
    /// Anything else: a panel, a plain shell, a Claude or Muse chat, a Muse shell. The
    /// project's default Codex account stands in, as it always has.
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UsageSource {
    Codex,
    Claude,
    /// One Grok session's tokens and cost: Grok's allowance is not readable, so this is never
    /// an account level, and only Current chat shows it.
    GrokSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Freshness {
    Current,
    /// Read longer ago than a refresh takes, or the last refresh failed.
    Stale,
    /// Not read yet.
    Loading,
    /// Nothing to show: the last read failed, or the account cannot be told.
    Unavailable,
}

/// One readout of the usage item.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct UsageSegment {
    pub(super) source: UsageSource,
    /// Whose subscription the figures are (Codex).
    pub(super) account: Option<String>,
    /// The focused session's account number, as its tab shows it.
    pub(super) number: Option<usize>,
    /// The windows left (`5h 80% left / 7d 60% left`), or a word for what stands in for them.
    pub(super) figures: String,
    pub(super) freshness: Freshness,
    /// Why the figures are missing or old.
    pub(super) note: Option<String>,
}

impl UsageSegment {
    fn mark(&self) -> &'static str {
        if self.freshness == Freshness::Stale {
            "*"
        } else {
            ""
        }
    }

    /// `CODEX`, with the account's number when its tab has one, and the stale mark.
    fn codex_tag(&self) -> String {
        match self.number {
            Some(number) => format!("CODEX A{number}{}", self.mark()),
            None => format!("CODEX{}", self.mark()),
        }
    }

    /// The readout as one line: Current chat's whole item, and each segment's hint.
    pub(super) fn text(&self) -> String {
        match (self.source, &self.account) {
            (UsageSource::Codex, Some(account)) => {
                format!("{} · {account} · {}", self.codex_tag(), self.figures)
            }
            (UsageSource::Codex, None) => format!("CODEX · {}", self.figures),
            (UsageSource::Claude, _) => format!("CLAUDE{} · {}", self.mark(), self.figures),
            (UsageSource::GrokSession, _) => self.figures.clone(),
        }
    }
}

/// Everything the usage item reads, all of it sampled already.
pub(super) struct UsageInputs<'a> {
    pub(super) mode: UsageMode,
    pub(super) focus: UsageFocus,
    /// The account new Codex sessions of the project get.
    pub(super) project_default: Selection,
    /// Every known Codex account (`known_codex_profiles`); read only in All accounts.
    pub(super) accounts: Vec<CodexProfile>,
    pub(super) codex: &'a BTreeMap<PathBuf, CodexUsageEntry>,
    /// Claude's status-line reads, by shell. Every one is the same subscription.
    pub(super) claude: &'a BTreeMap<String, ProviderUsage>,
    pub(super) grok: &'a BTreeMap<String, usage::GrokTabUsage>,
    pub(super) now: u64,
}

/// The usage item's readouts. Current chat has one: the focused session's, or the project's
/// default Codex account's. All accounts has one per known Codex account and, once a Claude
/// session has reported it, Claude's subscription. Neither reads out an allowance for Grok or
/// Muse, which have none to read.
pub(super) fn usage_segments(inputs: &UsageInputs) -> Vec<UsageSegment> {
    match inputs.mode {
        UsageMode::CurrentChat => vec![match &inputs.focus {
            UsageFocus::ClaudeShell(id) => match inputs.claude.get(id) {
                Some(snapshot) => claude_segment(snapshot, false, inputs.now),
                None => UsageSegment {
                    source: UsageSource::Claude,
                    account: None,
                    number: None,
                    figures: "WAITING".to_owned(),
                    freshness: Freshness::Loading,
                    note: Some("Claude reports usage after its first response".to_owned()),
                },
            },
            UsageFocus::GrokShell { id, alive } => UsageSegment {
                source: UsageSource::GrokSession,
                account: None,
                number: None,
                figures: if *alive {
                    usage::grok_chip_label(inputs.grok.get(id))
                } else {
                    "GROK · EXITED".to_owned()
                },
                freshness: Freshness::Current,
                note: None,
            },
            UsageFocus::Codex { account, number } => UsageSegment {
                number: *number,
                ..codex_segment(account, inputs, false)
            },
            UsageFocus::Other => codex_segment(&inputs.project_default, inputs, false),
        }],
        UsageMode::AllAccounts => {
            let mut segments: Vec<_> = inputs
                .accounts
                .iter()
                .map(|profile| codex_segment(&Selection::Account(profile.clone()), inputs, true))
                .collect();
            if segments.is_empty() {
                // Not even the system default's home could be found.
                segments.push(codex_segment(&Selection::Unavailable, inputs, true));
            }
            segments.extend(
                claude_subscription(inputs.claude)
                    .map(|snapshot| claude_segment(snapshot, true, inputs.now)),
            );
            segments
        }
    }
}

fn codex_segment(selection: &Selection, inputs: &UsageInputs, compact: bool) -> UsageSegment {
    let profile = match selection {
        Selection::Account(profile) => profile,
        Selection::Unknown | Selection::Unavailable => {
            return UsageSegment {
                source: UsageSource::Codex,
                account: None,
                number: None,
                figures: if *selection == Selection::Unknown {
                    "ACCOUNT UNKNOWN"
                } else {
                    "ACCOUNT UNAVAILABLE"
                }
                .to_owned(),
                freshness: Freshness::Unavailable,
                note: None,
            };
        }
    };
    let entry = inputs.codex.get(&profile.home);
    let (figures, freshness, note) = match entry {
        Some(CodexUsageEntry {
            codex: Some(snapshot),
            codex_error,
            ..
        }) => (
            limits(snapshot, compact),
            if codex_error.is_some() || stale(snapshot, inputs.now) {
                Freshness::Stale
            } else {
                Freshness::Current
            },
            codex_error
                .as_ref()
                .map(|error| format!("Last refresh failed: {error}")),
        ),
        Some(CodexUsageEntry {
            codex_error: Some(error),
            pending: false,
            ..
        }) => (
            "UNAVAILABLE".to_owned(),
            Freshness::Unavailable,
            Some(error.clone()),
        ),
        // Asked for and not answered, or not asked yet: the next refresh asks.
        _ => ("LOADING".to_owned(), Freshness::Loading, None),
    };
    UsageSegment {
        source: UsageSource::Codex,
        account: Some(profile.label.clone()),
        number: None,
        figures,
        freshness,
        note,
    }
}

fn claude_segment(snapshot: &ProviderUsage, compact: bool, now: u64) -> UsageSegment {
    UsageSegment {
        source: UsageSource::Claude,
        account: None,
        number: None,
        figures: limits(snapshot, compact),
        freshness: if stale(snapshot, now) {
            Freshness::Stale
        } else {
            Freshness::Current
        },
        note: None,
    }
}

/// Claude's subscription as its sessions last reported it: the newest report with quota
/// windows, else the newest report (an API key reports none).
fn claude_subscription(claude: &BTreeMap<String, ProviderUsage>) -> Option<&ProviderUsage> {
    claude
        .values()
        .filter(|snapshot| !snapshot.windows.is_empty())
        .max_by_key(|snapshot| snapshot.updated_at_unix)
        .or_else(|| {
            claude
                .values()
                .max_by_key(|snapshot| snapshot.updated_at_unix)
        })
}

/// Older than a refresh takes: the figures may have moved since.
fn stale(snapshot: &ProviderUsage, now: u64) -> bool {
    now.saturating_sub(snapshot.updated_at_unix) > 15 * 60
}

/// The first two windows and what is left of each: `5h 80% left / 7d 60% left`, or
/// `5h 80% / 7d 60% left` compact.
fn limits(snapshot: &ProviderUsage, compact: bool) -> String {
    let windows: Vec<_> = snapshot
        .windows
        .iter()
        .take(2)
        .map(|window| {
            (
                window.label.as_str(),
                (100.0 - window.used_percent).clamp(0.0, 100.0),
            )
        })
        .collect();
    if windows.is_empty() {
        return "—".to_owned();
    }
    let parts = windows.iter().map(|(label, left)| {
        if compact {
            format!("{label} {left:.0}%")
        } else {
            format!("{label} {left:.0}% left")
        }
    });
    let joined = parts.collect::<Vec<_>>().join(" / ");
    if compact {
        format!("{joined} left")
    } else {
        joined
    }
}

// ---- The default account menu's choices ---------------------------------------------------

/// One row of the Codex account menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DefaultChoiceRow {
    pub(super) choice: ProjectCodexAccount,
    pub(super) label: String,
    /// The account the row comes to, or why it cannot be chosen.
    pub(super) detail: Option<String>,
    pub(super) selectable: bool,
    /// The project's saved choice.
    pub(super) current: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DefaultChoices {
    /// Where the list of saved accounts stands, when it is not simply there.
    pub(super) status: Option<String>,
    pub(super) rows: Vec<DefaultChoiceRow>,
}

/// The choices Project Settings offers for `stored`, as menu rows: Inherit, annotated with
/// the account the app's selection comes to; System default, with the account Codex reported
/// for it (`system_label`); then every saved account. Before a list is read there are no saved
/// rows, only where the list stands. A saved choice the list does not have stays, unselectable,
/// as Project Settings keeps it.
pub(super) fn default_choices(
    stored: &ProjectCodexAccount,
    app_selected: Option<&str>,
    accounts: &settings::CodexAccountsState,
    system_label: Option<&str>,
) -> DefaultChoices {
    let snapshot = accounts.snapshot.as_ref();
    let system = |label: &str| match system_label {
        Some(account) => format!("{label} · {account}"),
        None => label.to_owned(),
    };
    let app_system = app_selected.is_none_or(|id| id == SYSTEM_DEFAULT_ID);
    let inherit = match resolve_selection(app_selected, snapshot) {
        Selection::Account(_) if app_system => format!("Uses {}", system(SYSTEM_LABEL)),
        Selection::Account(profile) => format!("Uses {}", profile.label),
        Selection::Unknown => "Uses the app's account · checking accounts…".to_owned(),
        Selection::Unavailable => "The app's account is unavailable".to_owned(),
    };
    let mut rows = vec![
        DefaultChoiceRow {
            choice: ProjectCodexAccount::Inherit,
            label: "Inherit app default".to_owned(),
            detail: Some(inherit),
            selectable: true,
            current: *stored == ProjectCodexAccount::Inherit,
        },
        DefaultChoiceRow {
            choice: ProjectCodexAccount::SystemDefault,
            label: SYSTEM_LABEL.to_owned(),
            detail: system_label.map(str::to_owned),
            selectable: true,
            current: *stored == ProjectCodexAccount::SystemDefault,
        },
    ];
    let status = match snapshot {
        None if accounts.pending => Some("Reading saved accounts…".to_owned()),
        None => Some("Saved accounts have not been read".to_owned()),
        Some(snapshot) => snapshot.error.clone(),
    };
    if let Some(snapshot) = snapshot {
        rows.extend(
            snapshot
                .accounts
                .iter()
                .filter(|account| !account.is_system_default)
                .map(|account| {
                    let choice = ProjectCodexAccount::Saved(account.id.clone());
                    DefaultChoiceRow {
                        current: *stored == choice,
                        choice,
                        label: account.label.clone(),
                        detail: (!account.available).then(|| {
                            account
                                .unavailable_reason
                                .clone()
                                .unwrap_or_else(|| "Unavailable".to_owned())
                        }),
                        selectable: account.available,
                    }
                }),
        );
    }
    if let ProjectCodexAccount::Saved(id) = stored
        && !rows.iter().any(|row| row.choice == *stored)
    {
        let short: String = id.chars().take(8).collect();
        rows.push(DefaultChoiceRow {
            choice: stored.clone(),
            label: if snapshot.is_some() {
                format!("Saved account unavailable · {short}")
            } else {
                format!("Saved account · {short}")
            },
            detail: Some(if snapshot.is_some() {
                "Choose another before starting Codex".to_owned()
            } else {
                "Checking whether it is still saved…".to_owned()
            }),
            selectable: false,
            current: true,
        });
    }
    DefaultChoices { status, rows }
}

// ---- The window's side --------------------------------------------------------------------

impl Workspace {
    fn project(&self) -> Option<&Project> {
        self.state
            .projects
            .iter()
            .find(|project| project.id == self.project_id)
    }

    /// The account new Codex sessions of this project get.
    pub(super) fn project_codex_selection(&self, cx: &App) -> Selection {
        project_selection(
            &self
                .project()
                .map(|project| project.codex_account.clone())
                .unwrap_or_default(),
            self.settings.selected_codex_account.as_deref(),
            cx.global::<settings::CodexAccountsState>()
                .snapshot
                .as_ref(),
        )
    }

    /// What the focused tab runs: a shell by its harness and frozen home, a chat by what its
    /// host last said about it (`ChatInfo`), or what the host listed.
    pub(super) fn usage_focus(&self, cx: &App) -> UsageFocus {
        let snapshot = cx
            .global::<settings::CodexAccountsState>()
            .snapshot
            .as_ref();
        let Some(tab) = self
            .panes
            .get(&self.active_pane)
            .and_then(|pane| pane.tabs.get(pane.active))
        else {
            return UsageFocus::Other;
        };
        if let Some(shell) = tab
            .shell_id()
            .and_then(|id| self.shells.iter().find(|shell| shell.id == id))
        {
            return match shell.harness {
                Some(HarnessKind::Codex) => UsageFocus::Codex {
                    account: shell_selection(shell, snapshot),
                    number: codex_account_numbers(&self.shells).get(&shell.id).copied(),
                },
                Some(HarnessKind::Claude) => UsageFocus::ClaudeShell(shell.id.clone()),
                Some(HarnessKind::Grok) => UsageFocus::GrokShell {
                    id: shell.id.clone(),
                    alive: shell.alive,
                },
                Some(HarnessKind::Muse) | None => UsageFocus::Other,
            };
        }
        let viewed = tab.chat().and_then(|view| view.read(cx).info().cloned());
        let listed = || {
            let id = tab.chat_id()?;
            self.shared_chats
                .iter()
                .chain(&self.chat_orchestrators)
                .flatten()
                .find(|chat| chat.id == id)
                .cloned()
        };
        match viewed.or_else(listed) {
            // The chat keeps the account it was made with; none is the system default.
            Some(info) if info.provider == Provider::Codex => UsageFocus::Codex {
                account: resolve_selection(info.codex_account_id.as_deref(), snapshot),
                number: None,
            },
            _ => UsageFocus::Other,
        }
    }

    pub(super) fn render_usage_chip(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let mode = self.settings.status_bar.usage_mode;
        let focus = self.usage_focus(cx);
        let project_default = self.project_codex_selection(cx);
        let accounts = match mode {
            UsageMode::CurrentChat => Vec::new(),
            UsageMode::AllAccounts => known_codex_profiles(
                cx.global::<settings::CodexAccountsState>()
                    .snapshot
                    .as_ref(),
            ),
        };
        let segments = usage_segments(&UsageInputs {
            mode,
            focus,
            project_default,
            accounts,
            codex: &cx.global::<AccountUsage>().codex,
            claude: &self.claude_usage,
            grok: &self.grok_usage,
            now: unix_time(),
        });
        let text = segments
            .iter()
            .map(UsageSegment::text)
            .collect::<Vec<_>>()
            .join("; ");
        let hint = segments
            .iter()
            .map(|segment| match &segment.note {
                Some(note) => format!("{} ({note})", segment.text()),
                None => segment.text(),
            })
            .chain(["Click for account usage".to_owned()])
            .collect::<Vec<_>>()
            .join("\n");
        // The parts are the item's own children, so a short bar shrinks every account label to
        // its ellipsis before any figures give way (to one of their own); the item clips, so
        // nothing runs into its neighbours.
        let item = behavior_controls::action("usage-chip", format!("Usage {text}"), colors)
            .relative()
            .flex()
            .items_center()
            .min_w(ui_text::space(48.0))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_color(rgb(status_accent(colors.cyan, colors)))
            .hover(|style| style.text_color(rgb(colors.magenta)));
        let item = match mode {
            UsageMode::CurrentChat => item
                .gap(ui_text::space(4.0))
                .children(segments.first().map(current_readout).into_iter().flatten()),
            UsageMode::AllAccounts => {
                let codex = segments
                    .iter()
                    .any(|segment| segment.source == UsageSource::Codex);
                item.gap(ui_text::space(4.0))
                    .children(codex.then(|| div().flex_none().child(ui_text::quiet("CODEX"))))
                    .children(
                        segments
                            .iter()
                            .enumerate()
                            .flat_map(|(index, segment)| compact_readout(index, segment)),
                    )
            }
        };
        item.child(tooltip::anchor(hint, Look::Status))
            .on_click(cx.listener(|workspace, _, window, cx| {
                workspace.open_panel(PanelKind::Usage, workspace.active_pane, window, cx);
            }))
            .into_any_element()
    }

    /// The Codex account item: the account new Codex chats and shells of this project get, and
    /// where that comes from. A click opens the menu that chooses it, another closes it.
    pub(super) fn render_default_account_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let open = self.account_menu_open;
        let snapshot = cx
            .global::<settings::CodexAccountsState>()
            .snapshot
            .as_ref();
        let system_home = resolve_selection(None, snapshot);
        let label = project_default_account_label(
            self.project(),
            self.settings.selected_codex_account.as_deref(),
            snapshot,
            reported_account_label(
                &cx.global::<AccountUsage>().codex,
                system_home_path(&system_home),
            )
            .as_deref(),
        );
        let bounds = self.account_item_bounds.clone();
        behavior_controls::action(
            "status-codex-account",
            format!("New Codex sessions: {label}"),
            colors,
        )
        .aria_expanded(open)
        .relative()
        .max_w(ui_text::space(300.0))
        .min_w(ui_text::space(48.0))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_color(rgb(if open {
            colors.cyan
        } else {
            status_accent(colors.cyan, colors)
        }))
        .when(open, |item| item.bg(rgb(colors.panel_active)))
        .hover(|style| style.text_color(rgb(colors.magenta)))
        .child(ui_text::quiet(label))
        .child(
            canvas(move |area, _, _| bounds.set(area), |_, _, _, _| {})
                .absolute()
                .inset_0(),
        )
        .when(!open, |item| {
            item.child(tooltip::anchor(
                "New Codex sessions in this project · click to choose",
                Look::Status,
            ))
        })
        .on_click(cx.listener(|workspace, _, window, cx| {
            workspace.toggle_account_menu(window, cx);
        }))
        .into_any_element()
    }

    /// Whether the status bar shows the Codex account item, which the account menu hangs from.
    /// A remote project's bar draws no account item.
    pub(super) fn account_item_shown(&self) -> bool {
        use status_bar::{StatusItemKind, StatusSide};
        !self.is_remote()
            && [StatusSide::Left, StatusSide::Right]
                .into_iter()
                .any(|side| {
                    self.settings
                        .status_bar
                        .visible_items(side)
                        .contains(&StatusItemKind::CodexAccount)
                })
    }

    pub(super) fn toggle_account_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.account_menu_open {
            self.close_account_menu(window, cx);
            return;
        }
        if self.modal_open() {
            return;
        }
        self.dismiss_project_sort_menus(window, cx);
        if self.panel_menu.is_none() && !self.layout_menu_open && !self.global_menu_open {
            self.menu_return.capture(window, cx);
        }
        self.panel_menu = None;
        self.layout_menu_open = false;
        self.global_menu_open = false;
        self.account_menu_open = true;
        // Terminals are native views drawn above everything of ours; the menu is drawn over
        // their snapshots instead, as the layout menu is.
        if !self.tab_dragging {
            self.begin_tab_drag(cx);
        }
        self.menu_focus.focus(window, cx);
        // The saved accounts are the choices; with no list yet, ask for it as Settings do.
        if cx
            .global::<settings::CodexAccountsState>()
            .snapshot
            .is_none()
        {
            settings::refresh_codex_accounts(cx);
        }
        cx.notify();
    }

    pub(super) fn close_account_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.account_menu_open {
            return;
        }
        self.account_menu_open = false;
        self.finish_tab_drag(cx);
        if !self.menu_return.restore_within(&self.focus, window, cx) {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// Save `choice` as this project's Codex account, as Project Settings does, and show it
    /// at once here and in an open Project Settings. A choice the store refuses changes
    /// nothing, and says why.
    pub(super) fn choose_project_codex_account(
        &mut self,
        choice: ProjectCodexAccount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self
            .store
            .set_project_codex_account(&self.project_id, choice)
        {
            Ok(project) => {
                // As Project Settings' save says, and so an earlier refusal does not stay.
                self.notice = Some(format!("Saved {}", project.name));
                if let Some(saved) = self
                    .state
                    .projects
                    .iter_mut()
                    .find(|saved| saved.id == project.id)
                {
                    *saved = project;
                }
                if let Some(panel) = &self.project_settings_panel {
                    panel.update(cx, |panel, cx| panel.refresh_folders(window, cx));
                }
            }
            Err(error) => self.notice = Some(error),
        }
        cx.notify();
    }

    /// The account menu, above the status bar at the Codex account item: the choices of
    /// Project Settings' Codex account, the saved one marked, and Project Settings itself.
    pub(super) fn render_account_menu(
        &self,
        window_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme::palette(cx);
        let width = ui_text::space_f32(320.0).min((window_width - 12.0).max(0.0));
        let item = self.account_item_bounds.get();
        let left = layout_menu_left(
            item.right().as_f32(),
            item.size.width.as_f32(),
            width,
            window_width,
        );
        let accounts = cx.global::<settings::CodexAccountsState>().clone();
        let system_home = resolve_selection(None, accounts.snapshot.as_ref());
        let system_label = reported_account_label(
            &cx.global::<AccountUsage>().codex,
            system_home_path(&system_home),
        );
        let choices = default_choices(
            &self
                .project()
                .map(|project| project.codex_account.clone())
                .unwrap_or_default(),
            self.settings.selected_codex_account.as_deref(),
            &accounts,
            system_label.as_deref(),
        );
        let note = |text: String, color: u32| {
            div()
                .px(ui_text::space(8.0))
                .pb(ui_text::space(4.0))
                .text_size(ui_text::text(9.0))
                .text_color(rgb(color))
                .child(text)
        };
        let rows: Vec<_> = choices
            .rows
            .into_iter()
            .map(|row| self.account_menu_row(row, cx))
            .collect();
        behavior_controls::focus_scope(
            div()
                .id("account-menu")
                .role(gpui::Role::Menu)
                .aria_label("New Codex sessions in this project")
                .absolute()
                .left(px(left))
                .bottom(ui_text::space(STATUS_BAR_HEIGHT) + px(3.0))
                .w(px(width))
                .bg(rgb(colors.panel_active))
                .border_1()
                .border_color(rgb(colors.magenta))
                .p(ui_text::space(3.0))
                .map(|menu| controls::native(menu, |menu| controls::menu(menu, colors)))
                .occlude()
                .on_mouse_down_out(cx.listener(
                    |workspace, event: &gpui::MouseDownEvent, window, cx| {
                        // A click on the item itself is its own toggle.
                        if !workspace
                            .account_item_bounds
                            .get()
                            .contains(&event.position)
                        {
                            workspace.close_account_menu(window, cx);
                        }
                    },
                ))
                .child(pane_menu_heading(
                    "New Codex sessions in this project",
                    true,
                    colors,
                ))
                .child(note(
                    "Running chats and shells keep their account; --account still chooses one."
                        .to_owned(),
                    colors.muted,
                ))
                .children(choices.status.map(|status| note(status, colors.gold)))
                .children(rows)
                .child(pane_menu_heading("Project", false, colors))
                .child(
                    behavior_controls::action(
                        "account-menu-project-settings",
                        "Project Settings…",
                        colors,
                    )
                    .role(gpui::Role::MenuItem)
                    .flex()
                    .items_center()
                    .gap(ui_text::space(8.0))
                    .px(ui_text::space(8.0))
                    .py(ui_text::space(5.0))
                    .text_size(ui_text::text(10.0))
                    .map(|item| controls::native(item, |item| controls::menu_row(item, colors)))
                    .text_color(rgb(colors.text))
                    .hover(move |style| {
                        controls::hovered(style, controls::menu_row_hover(colors), |style| {
                            style.bg(rgb(colors.divider)).text_color(rgb(colors.cyan))
                        })
                    })
                    .child(div().w(ui_text::space(14.0)).flex_none())
                    .child(ui_text::cased("Project Settings…"))
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.account_menu_open = false;
                        workspace.menu_return.forget();
                        workspace.finish_tab_drag(cx);
                        workspace.open_panel(
                            PanelKind::ProjectSettings,
                            workspace.active_pane,
                            window,
                            cx,
                        );
                    })),
                ),
            "account-menu-scope",
            &self.menu_focus,
        )
        .into_any_element()
    }

    /// A choice of the account menu: a radio item, marked when it is the project's saved
    /// choice, with the account it comes to beneath. No hover hint, as in the other menus.
    fn account_menu_row(&self, row: DefaultChoiceRow, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme::palette(cx);
        let id = match &row.choice {
            ProjectCodexAccount::Inherit => "account-menu-inherit".to_owned(),
            ProjectCodexAccount::SystemDefault => "account-menu-system".to_owned(),
            ProjectCodexAccount::Saved(id) => format!("account-menu-saved-{id}"),
        };
        let name = match &row.detail {
            Some(detail) => format!("{}, {detail}", row.label),
            None => row.label.clone(),
        };
        let enabled = row.selectable;
        let choice = row.choice.clone();
        behavior_controls::action(gpui::SharedString::from(id), name, colors)
            .role(gpui::Role::MenuItemRadio)
            .aria_toggled(if row.current {
                gpui::Toggled::True
            } else {
                gpui::Toggled::False
            })
            .disabled(!enabled)
            .flex()
            .flex_col()
            .gap(ui_text::space(2.0))
            .px(ui_text::space(8.0))
            .py(ui_text::space(5.0))
            .text_size(ui_text::text(10.0))
            .map(|item| controls::native(item, |item| controls::menu_row(item, colors)))
            .text_color(rgb(if enabled { colors.text } else { colors.muted }))
            .when(enabled, |item| {
                item.hover(move |style| {
                    controls::hovered(style, controls::menu_row_hover(colors), |style| {
                        style.bg(rgb(colors.divider)).text_color(rgb(colors.cyan))
                    })
                })
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui_text::space(8.0))
                    .child(
                        div()
                            .w(ui_text::space(14.0))
                            .flex_none()
                            .children(row.current.then(|| menu_icon(Icon::Check, colors))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(row.label),
                    ),
            )
            .children(row.detail.map(|detail| {
                div()
                    .pl(ui_text::space(22.0))
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(ui_text::text(9.0))
                    .text_color(rgb(colors.muted))
                    .child(detail)
            }))
            .when(enabled, |item| {
                item.on_click(cx.listener(move |workspace, _, window, cx| {
                    workspace.close_account_menu(window, cx);
                    workspace.choose_project_codex_account(choice.clone(), window, cx);
                }))
            })
            .into_any_element()
    }
}

fn system_home_path(selection: &Selection) -> Option<&Path> {
    match selection {
        Selection::Account(profile) => Some(&profile.home),
        Selection::Unavailable | Selection::Unknown => None,
    }
}

/// Current chat's one readout, as parts of the item. A Codex account's label gives way to its
/// figures when the bar is short.
fn current_readout(segment: &UsageSegment) -> Vec<AnyElement> {
    let Some(account) = segment
        .account
        .as_ref()
        .filter(|_| segment.source == UsageSource::Codex)
    else {
        return vec![figures(segment.text())];
    };
    vec![
        div()
            .flex_none()
            .child(ui_text::quiet(format!("{} ·", segment.codex_tag())))
            .into_any_element(),
        account_label(account, 160.0, 0.0),
        figures(format!("· {}", segment.figures)),
    ]
}

/// One account's readout in All accounts, as parts of the item: its label, which gives way
/// first, then its figures.
fn compact_readout(index: usize, segment: &UsageSegment) -> Vec<AnyElement> {
    // A wider space parts one account from the next.
    let lead = if index == 0 { 0.0 } else { 6.0 };
    let mut parts = Vec::new();
    match (&segment.account, segment.source) {
        (Some(account), _) => parts.push(account_label(account, 130.0, lead)),
        (None, UsageSource::Claude) => parts.push(
            div()
                .flex_none()
                .ml(ui_text::space(lead))
                .child(ui_text::quiet("CLAUDE"))
                .into_any_element(),
        ),
        (None, _) => {}
    }
    parts.push(figures(format!("{}{}", segment.mark(), segment.figures)));
    parts
}

/// An account's label: it shrinks first, to an ellipsis.
fn account_label(account: &str, max_width: f32, lead: f32) -> AnyElement {
    div()
        .ml(ui_text::space(lead))
        .min_w(ui_text::space(12.0))
        .max_w(ui_text::space(max_width))
        .overflow_hidden()
        .text_ellipsis()
        .child(account.to_owned())
        .into_any_element()
}

/// Figures: they keep their width until every label has given way, then end in an ellipsis
/// rather than a cut number.
fn figures(text: String) -> AnyElement {
    div()
        .flex_shrink(0.001)
        .min_w_0()
        .overflow_hidden()
        .text_ellipsis()
        .child(ui_text::quiet(text))
        .into_any_element()
}

#[cfg(test)]
mod tests;
