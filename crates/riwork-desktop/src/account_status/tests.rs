use super::*;
use crate::global_window::tests::{chat_info, shell};
use codex_accounts::CodexAccount;
use gpui_kit::test::TestWindowExt;
use usage::UsageWindow;

const NOW: u64 = 1_800_000_000;
const LONG_A: &str = "a.rather.long.mailbox.name@example.test · A workspace with a long name";
const TEAM_B: &str = "b@example.test · Team B";
/// What Codex reports for the system default's home, the only name that account has.
const SYSTEM_REPORTED: &str = "sys@example.test · plus";

fn account(id: &str, label: &str, home: &Path) -> CodexAccount {
    CodexAccount {
        id: id.to_owned(),
        label: label.to_owned(),
        email: label
            .split(" · ")
            .next()
            .filter(|email| email.contains('@'))
            .map(str::to_owned),
        home: home.to_owned(),
        available: true,
        unavailable_reason: None,
        is_system_default: id == SYSTEM_DEFAULT_ID,
    }
}

/// The system default, two saved accounts and a third whose home is gone.
fn accounts(root: &Path) -> AccountsSnapshot {
    let mut gone = account("account-c", "c@example.test", &root.join("c"));
    gone.available = false;
    gone.unavailable_reason = Some("This saved account's home is missing.".into());
    AccountsSnapshot {
        accounts: vec![
            account(SYSTEM_DEFAULT_ID, SYSTEM_LABEL, &root.join("system")),
            account("account-a", LONG_A, &root.join("a")),
            account("account-b", TEAM_B, &root.join("b")),
            gone,
        ],
        ..Default::default()
    }
}

fn profile(list: &AccountsSnapshot, id: &str) -> CodexProfile {
    let account = list
        .accounts
        .iter()
        .find(|account| account.id == id)
        .unwrap();
    CodexProfile {
        home: account.home.clone(),
        label: account.label.clone(),
    }
}

/// Two windows with `used` percent used, read at `updated`.
fn quota(provider: &str, used: [f64; 2], updated: u64) -> ProviderUsage {
    ProviderUsage {
        provider: provider.to_owned(),
        windows: vec![
            UsageWindow {
                label: "5h".into(),
                used_percent: used[0],
                resets_at: None,
                window_minutes: Some(300),
            },
            UsageWindow {
                label: "7d".into(),
                used_percent: used[1],
                resets_at: None,
                window_minutes: Some(10080),
            },
        ],
        updated_at_unix: updated,
        account_label: None,
        context_used_percent: None,
        session_cost_usd: None,
        session: None,
        session_error: None,
    }
}

fn read(snapshot: ProviderUsage) -> CodexUsageEntry {
    CodexUsageEntry {
        codex: Some(snapshot),
        ..Default::default()
    }
}

/// A read that failed just now, and will not be tried again for ten minutes.
fn failed(error: &str, at: u64) -> CodexUsageEntry {
    CodexUsageEntry {
        codex_error: Some(error.into()),
        last_attempt: at,
        failures: 6,
        ..Default::default()
    }
}

/// What a window has read, for the usage readouts.
#[derive(Default)]
struct Reads {
    codex: BTreeMap<PathBuf, CodexUsageEntry>,
    claude: BTreeMap<String, ProviderUsage>,
    grok: BTreeMap<String, usage::GrokTabUsage>,
}

impl Reads {
    fn segments(
        &self,
        mode: UsageMode,
        focus: UsageFocus,
        project_default: Selection,
        accounts: Vec<CodexProfile>,
    ) -> Vec<UsageSegment> {
        usage_segments(&UsageInputs {
            mode,
            focus,
            project_default,
            accounts,
            codex: &self.codex,
            claude: &self.claude,
            grok: &self.grok,
            now: NOW,
        })
    }

    fn current(&self, focus: UsageFocus, project_default: Selection) -> UsageSegment {
        let mut segments =
            self.segments(UsageMode::CurrentChat, focus, project_default, Vec::new());
        assert_eq!(segments.len(), 1, "Current chat has one readout");
        segments.remove(0)
    }
}

#[test]
fn current_chat_reads_the_focused_codex_account_else_the_project_default_and_names_it() {
    let list = accounts(Path::new("/fixture"));
    let (a, b) = (profile(&list, "account-a"), profile(&list, "account-b"));
    let mut reads = Reads::default();
    reads
        .codex
        .insert(a.home.clone(), read(quota("codex", [20.0, 40.0], NOW)));
    reads.codex.insert(
        b.home.clone(),
        CodexUsageEntry {
            pending: true,
            ..Default::default()
        },
    );

    // A focused Codex chat or shell reads out its own account, named, not the default.
    let focused = reads.current(
        UsageFocus::Codex {
            account: Selection::Account(a.clone()),
            number: None,
        },
        Selection::Account(b.clone()),
    );
    assert_eq!(focused.source, UsageSource::Codex);
    assert_eq!(focused.account.as_deref(), Some(LONG_A));
    assert_eq!(focused.freshness, Freshness::Current);
    assert_eq!(
        focused.text(),
        format!("CODEX · {LONG_A} · 5h 80% left / 7d 60% left")
    );
    // With more than one account running in the project, it carries its tab's number.
    let numbered = reads.current(
        UsageFocus::Codex {
            account: Selection::Account(a.clone()),
            number: Some(2),
        },
        Selection::Account(b.clone()),
    );
    assert_eq!(
        numbered.text(),
        format!("CODEX A2 · {LONG_A} · 5h 80% left / 7d 60% left")
    );

    // Anything else reads out the project's default, named as well: here still being read.
    let fallback = reads.current(UsageFocus::Other, Selection::Account(b.clone()));
    assert_eq!(fallback.freshness, Freshness::Loading);
    assert_eq!(fallback.text(), format!("CODEX · {TEAM_B} · LOADING"));

    // An account the list cannot tell is said to be so, not swapped for another.
    assert_eq!(
        reads
            .current(
                UsageFocus::Codex {
                    account: Selection::Unknown,
                    number: None,
                },
                Selection::Account(a.clone())
            )
            .text(),
        "CODEX · ACCOUNT UNKNOWN"
    );
    assert_eq!(
        reads
            .current(UsageFocus::Other, Selection::Unavailable)
            .text(),
        "CODEX · ACCOUNT UNAVAILABLE"
    );

    // Old figures, or a refresh that failed over good ones, are marked, and say why.
    reads.codex.insert(
        a.home.clone(),
        read(quota("codex", [20.0, 40.0], NOW - 16 * 60)),
    );
    let old = reads.current(
        UsageFocus::Codex {
            account: Selection::Account(a.clone()),
            number: None,
        },
        Selection::Unknown,
    );
    assert_eq!(old.freshness, Freshness::Stale);
    assert!(old.text().starts_with("CODEX* · "), "{}", old.text());
    reads.codex.insert(
        a.home.clone(),
        CodexUsageEntry {
            codex_error: Some("Codex is signed out".into()),
            ..read(quota("codex", [20.0, 40.0], NOW))
        },
    );
    let failed_over = reads.current(
        UsageFocus::Codex {
            account: Selection::Account(a),
            number: None,
        },
        Selection::Unknown,
    );
    assert_eq!(failed_over.freshness, Freshness::Stale);
    assert_eq!(
        failed_over.note.as_deref(),
        Some("Last refresh failed: Codex is signed out")
    );

    // Claude and Grok sessions read out as they did before the modes.
    reads
        .claude
        .insert("claude-1".into(), quota("claude", [50.0, 10.0], NOW));
    assert_eq!(
        reads
            .current(
                UsageFocus::ClaudeShell("claude-1".into()),
                Selection::Unknown
            )
            .text(),
        "CLAUDE · 5h 50% left / 7d 90% left"
    );
    assert_eq!(
        reads
            .current(
                UsageFocus::ClaudeShell("claude-2".into()),
                Selection::Unknown
            )
            .text(),
        "CLAUDE · WAITING"
    );
    let grok = |alive| UsageFocus::GrokShell {
        id: "grok-1".into(),
        alive,
    };
    assert_eq!(
        reads.current(grok(true), Selection::Unknown).text(),
        "GROK · LOADING"
    );
    assert_eq!(
        reads.current(grok(false), Selection::Unknown).text(),
        "GROK · EXITED"
    );
}

#[test]
fn all_accounts_lists_every_codex_account_keeps_unread_ones_and_adds_only_a_known_claude() {
    let list = accounts(Path::new("/fixture"));
    let profiles = known_codex_profiles(Some(&list));
    // The accounts whose usage is read, in the list's order: the unavailable one has none.
    assert_eq!(
        profiles
            .iter()
            .map(|profile| profile.label.as_str())
            .collect::<Vec<_>>(),
        [SYSTEM_LABEL, LONG_A, TEAM_B]
    );
    let mut reads = Reads::default();
    reads.codex.insert(
        profile(&list, SYSTEM_DEFAULT_ID).home,
        read(quota("codex", [10.0, 30.0], NOW)),
    );
    reads.codex.insert(
        profile(&list, "account-a").home,
        failed("Codex is signed out", NOW),
    );
    // Grok's session figures exist, and a Grok, Muse or any other tab is focused.
    reads
        .grok
        .insert("grok-1".into(), usage::GrokTabUsage::default());
    for focus in [
        UsageFocus::GrokShell {
            id: "grok-1".into(),
            alive: true,
        },
        UsageFocus::Other,
        UsageFocus::Codex {
            account: Selection::Account(profile(&list, "account-b")),
            number: None,
        },
    ] {
        let all = reads.segments(
            UsageMode::AllAccounts,
            focus,
            Selection::Account(profile(&list, "account-a")),
            profiles.clone(),
        );
        assert_eq!(
            all.iter()
                .map(|segment| (
                    segment.source,
                    segment.account.as_deref(),
                    segment.figures.as_str(),
                    segment.freshness
                ))
                .collect::<Vec<_>>(),
            [
                (
                    UsageSource::Codex,
                    Some(SYSTEM_LABEL),
                    "5h 90% / 7d 70% left",
                    Freshness::Current
                ),
                // A failed read keeps its account in view, and says it is unavailable.
                (
                    UsageSource::Codex,
                    Some(LONG_A),
                    "UNAVAILABLE",
                    Freshness::Unavailable
                ),
                // One never read yet waits for the next refresh.
                (
                    UsageSource::Codex,
                    Some(TEAM_B),
                    "LOADING",
                    Freshness::Loading
                ),
            ]
        );
        assert_eq!(all[1].note.as_deref(), Some("Codex is signed out"));
    }

    // Claude is one subscription, read out once a session has reported it: the newest report
    // with quota windows, not a newer one that bills an API key and has none.
    reads
        .claude
        .insert("older".into(), quota("claude", [50.0, 10.0], NOW - 60));
    let mut api_key = quota("claude", [0.0, 0.0], NOW);
    api_key.windows.clear();
    reads.claude.insert("newer".into(), api_key);
    let all = reads.segments(
        UsageMode::AllAccounts,
        UsageFocus::Other,
        Selection::Unknown,
        profiles.clone(),
    );
    assert_eq!(all.len(), 4);
    let claude = all.last().unwrap();
    assert_eq!(claude.source, UsageSource::Claude);
    assert_eq!(claude.account, None);
    assert_eq!(claude.figures, "5h 50% / 7d 90% left");
    assert!(
        all.iter()
            .all(|segment| matches!(segment.source, UsageSource::Codex | UsageSource::Claude)),
        "no Grok or Muse allowance is ever read out"
    );

    // Before any list is read, the system default is the one account known.
    assert_eq!(
        known_codex_profiles(None)
            .iter()
            .map(|profile| profile.label.as_str())
            .collect::<Vec<_>>(),
        [SYSTEM_LABEL]
    );
}

#[test]
fn the_default_choices_mark_the_saved_one_and_say_what_each_comes_to() {
    let list = accounts(Path::new("/fixture"));
    let state = settings::CodexAccountsState {
        snapshot: Some(list.clone()),
        pending: false,
    };
    let rows = |stored: &ProjectCodexAccount, app: Option<&str>, system: Option<&str>| {
        default_choices(stored, app, &state, system)
    };

    // Inherit names the account the app's choice comes to; System default what Codex said.
    let choices = rows(
        &ProjectCodexAccount::Inherit,
        Some("account-a"),
        Some(SYSTEM_REPORTED),
    );
    assert_eq!(choices.status, None);
    assert_eq!(
        choices
            .rows
            .iter()
            .map(|row| (
                row.label.as_str(),
                row.detail.as_deref(),
                row.selectable,
                row.current
            ))
            .collect::<Vec<_>>(),
        [
            (
                "Inherit app default",
                Some(format!("Uses {LONG_A}").as_str()),
                true,
                true
            ),
            (SYSTEM_LABEL, Some(SYSTEM_REPORTED), true, false),
            (LONG_A, None, true, false),
            (TEAM_B, None, true, false),
            // A saved account without its home can be seen, not chosen anew.
            (
                "c@example.test",
                Some("This saved account's home is missing."),
                false,
                false
            ),
        ]
    );
    assert_eq!(
        choices.rows[2].choice,
        ProjectCodexAccount::Saved("account-a".into())
    );

    // The app on the system default: Inherit says so, with Codex's name for it when known.
    assert_eq!(
        rows(
            &ProjectCodexAccount::SystemDefault,
            None,
            Some(SYSTEM_REPORTED)
        )
        .rows[0]
            .detail
            .as_deref(),
        Some("Uses System default · sys@example.test · plus")
    );
    let system = rows(&ProjectCodexAccount::SystemDefault, None, None);
    assert_eq!(
        system.rows[0].detail.as_deref(),
        Some("Uses System default")
    );
    assert_eq!(system.rows[1].detail, None);
    assert!(system.rows[1].current && !system.rows[0].current);
    assert_eq!(
        rows(&ProjectCodexAccount::Inherit, Some("account-c"), None).rows[0]
            .detail
            .as_deref(),
        Some("The app's account is unavailable")
    );

    // A saved choice the list no longer has stays marked, as Project Settings keeps it, and
    // cannot be chosen again.
    let gone = ProjectCodexAccount::Saved("removed-account".into());
    let kept = rows(&gone, None, None);
    let last = kept.rows.last().unwrap();
    assert_eq!(last.label, "Saved account unavailable · removed-");
    assert!(last.current && !last.selectable);
    assert_eq!(kept.rows.iter().filter(|row| row.current).count(), 1);
    // An unavailable saved choice is the one marked, and still unselectable.
    let unavailable = rows(&ProjectCodexAccount::Saved("account-c".into()), None, None);
    assert!(
        unavailable
            .rows
            .iter()
            .any(|row| row.current && !row.selectable && row.label == "c@example.test")
    );

    // With no list read yet: where it stands, and no saved account made up.
    for (pending, status) in [
        (true, "Reading saved accounts…"),
        (false, "Saved accounts have not been read"),
    ] {
        let unread = settings::CodexAccountsState {
            snapshot: None,
            pending,
        };
        let choices = default_choices(&gone, Some("account-a"), &unread, None);
        assert_eq!(choices.status.as_deref(), Some(status));
        assert_eq!(
            choices
                .rows
                .iter()
                .map(|row| (row.choice.clone(), row.selectable, row.current))
                .collect::<Vec<_>>(),
            [
                (ProjectCodexAccount::Inherit, true, false),
                (ProjectCodexAccount::SystemDefault, true, false),
                (gone.clone(), false, true),
            ]
        );
        assert_eq!(choices.rows[2].label, "Saved account · removed-");
        assert_eq!(
            choices.rows[0].detail.as_deref(),
            Some("Uses the app's account · checking accounts…")
        );
    }
    // A failed discovery says why above the choices.
    let mut errored = list;
    errored.error = Some("Orca's CLI is unavailable.".into());
    let errored = settings::CodexAccountsState {
        snapshot: Some(errored),
        pending: false,
    };
    assert_eq!(
        default_choices(&ProjectCodexAccount::Inherit, None, &errored, None)
            .status
            .as_deref(),
        Some("Orca's CLI is unavailable.")
    );
}

#[test]
fn the_project_default_resolves_as_a_launch_does_and_the_item_names_it() {
    let list = accounts(Path::new("/fixture"));
    let a = profile(&list, "account-a");
    let system = profile(&list, SYSTEM_DEFAULT_ID);
    let resolve = |choice: ProjectCodexAccount, app: Option<&str>| {
        project_selection(&choice, app, Some(&list))
    };
    assert_eq!(
        resolve(ProjectCodexAccount::Inherit, Some("account-a")),
        Selection::Account(a.clone())
    );
    assert_eq!(
        resolve(ProjectCodexAccount::Inherit, Some(SYSTEM_DEFAULT_ID)),
        Selection::Account(system.clone())
    );
    assert_eq!(
        resolve(ProjectCodexAccount::SystemDefault, Some("account-a")),
        Selection::Account(system)
    );
    assert_eq!(
        resolve(ProjectCodexAccount::Saved("account-a".into()), None),
        Selection::Account(a)
    );
    assert_eq!(
        resolve(ProjectCodexAccount::Saved("account-c".into()), None),
        Selection::Unavailable
    );
    assert_eq!(
        project_selection(&ProjectCodexAccount::Saved("account-a".into()), None, None),
        Selection::Unknown
    );

    let mut project: Project = serde_json::from_value(serde_json::json!({
        "id":"alpha", "name":"Alpha", "root":"/alpha", "created_at":1
    }))
    .unwrap();
    project.codex_account = ProjectCodexAccount::SystemDefault;
    assert_eq!(
        project_default_account_label(Some(&project), None, Some(&list), Some(SYSTEM_REPORTED)),
        "DEFAULT (SYSTEM) · System default · sys@example.test · plus"
    );
}

#[test]
fn a_codex_shell_is_named_by_the_account_of_its_frozen_home() {
    let list = accounts(Path::new("/fixture"));
    let mut session = shell("codex-1", ShellKind::Project, Some("alpha"));
    session.codex_home = Some(profile(&list, "account-b").home);
    session.codex_account_id = Some("account-b".into());
    assert_eq!(
        shell_selection(&session, Some(&list)),
        Selection::Account(profile(&list, "account-b"))
    );
    // A home the list no longer has keeps the name it was started with.
    session.codex_home = Some(PathBuf::from("/fixture/removed"));
    session.codex_account_label = Some("old@example.test · Old".into());
    assert_eq!(
        shell_selection(&session, Some(&list)),
        Selection::Account(CodexProfile {
            home: PathBuf::from("/fixture/removed"),
            label: "old@example.test · Old".into(),
        })
    );
    session.codex_home = None;
    assert_eq!(shell_selection(&session, Some(&list)), Selection::Unknown);
}

// ---- The window ---------------------------------------------------------------------------

struct Fixture {
    handle: gpui::AnyWindowHandle,
    workspace: Entity<Workspace>,
    home: PathBuf,
    project_id: String,
    store: Store,
    list: AccountsSnapshot,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Usage the window already has for `list`'s accounts. Each is being read, or failed just
/// now, so no test asks Codex for real: the system default and the first saved account have
/// figures, every other account failed.
fn usage_of(list: &AccountsSnapshot) -> AccountUsage {
    let now = unix_time();
    let codex = list
        .accounts
        .iter()
        .map(|account| {
            let entry = match account.id.as_str() {
                SYSTEM_DEFAULT_ID => CodexUsageEntry {
                    codex: Some(ProviderUsage {
                        account_label: Some(SYSTEM_REPORTED.into()),
                        ..quota("codex", [10.0, 30.0], now)
                    }),
                    pending: true,
                    ..Default::default()
                },
                "account-a" => CodexUsageEntry {
                    codex: Some(quota("codex", [20.0, 40.0], now)),
                    pending: true,
                    ..Default::default()
                },
                _ => failed("Codex is signed out", now),
            };
            (account.home.clone(), entry)
        })
        .collect();
    AccountUsage { codex }
}

/// A local project's window in a private data directory, with the project saved in its store,
/// `list` as the account list, usage for it (`usage_of`) and the usage item in `mode`. A
/// locked navigation pane (1, focused) shows Projects beside a work pane (2) showing Usage.
fn window(
    cx: &mut gpui::TestAppContext,
    list: Option<AccountsSnapshot>,
    mode: UsageMode,
) -> Fixture {
    let home = std::env::temp_dir().join(format!("rw-accounts-{}", uuid::Uuid::new_v4()));
    let root = home.join("work");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(&home).unwrap();
    let project = store.add_project(&root, Some("Fixture")).unwrap();
    let project_id = project.id.clone();
    let list = list.unwrap_or_else(|| accounts(&home.join("accounts")));
    let usage = usage_of(&list);
    SettingsStore::open(&home)
        .unwrap()
        .update(|settings| settings.status_bar.usage_mode = mode)
        .unwrap();
    let accounts_state = settings::CodexAccountsState {
        snapshot: Some(list.clone()),
        pending: false,
    };
    cx.update(|cx| {
        cx.set_global(Settings::default());
        cx.set_global(Appearance::resolve(theme::ThemeChoice::Native, false));
        cx.set_global(accounts_state);
        cx.set_global(RemoteState::default());
        cx.set_global(usage);
        cx.set_global(CuaSetupState::default());
        ui_text::init(cx);
        tooltip::init(cx);
    });
    let startup = WorkspaceStartup {
        store: store.clone(),
        layouts: LayoutStore::open(&home).unwrap(),
        settings_store: SettingsStore::open(&home).unwrap(),
        sessions: SessionManager::at(home.clone()).unwrap(),
        state: store.snapshot().unwrap(),
        project,
        remote: None,
    };
    let (handle, workspace) = crate::form_input::test_window(cx, move |window, cx| {
        Workspace::build(startup, None, window, cx, false)
    });
    // The window's settings are the saved ones, as at launch.
    let saved = SettingsStore::open(&home).unwrap().load().unwrap();
    cx.update(|cx| cx.set_global(saved.clone()));
    crate::form_input::test_turn(cx, handle, |_, cx| {
        workspace.update(cx, |workspace, _| {
            workspace.settings = saved;
            workspace.panes = BTreeMap::from([
                (
                    1,
                    Pane {
                        tabs: vec![new_panel_tab(
                            1,
                            Workspace::panel_title(PanelKind::Projects),
                            PanelKind::Projects,
                        )],
                        active: 0,
                    },
                ),
                (
                    2,
                    Pane {
                        tabs: vec![new_panel_tab(
                            2,
                            Workspace::panel_title(PanelKind::Usage),
                            PanelKind::Usage,
                        )],
                        active: 0,
                    },
                ),
            ]);
            workspace.layout = Layout::navigation_beside(1, Layout::Pane(2));
            workspace.active_pane = 1;
            workspace.main_pane = None;
            workspace.locked_panes = Some(HashSet::from([1]));
            workspace.next_tab_id = 3;
            workspace.next_pane_id = 3;
            workspace.shared_tabs = None;
            workspace.layout_ready = true;
        });
    });
    fixture_settled(cx, &workspace, mode);
    Fixture {
        handle,
        workspace,
        home,
        project_id,
        store,
        list,
    }
}

fn fixture_settled(cx: &mut gpui::TestAppContext, workspace: &Entity<Workspace>, mode: UsageMode) {
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.settings.status_bar.usage_mode, mode);
    });
}

/// Change the saved settings and the window's alike, as Settings does.
fn change_settings(
    cx: &mut gpui::TestAppContext,
    fixture: &Fixture,
    change: impl FnOnce(&mut Settings),
) {
    let saved = SettingsStore::open(&fixture.home)
        .unwrap()
        .update(change)
        .unwrap();
    // A test window starts no services, so nothing carries the global to the window.
    cx.update(|cx| cx.set_global(saved.clone()));
    fixture.workspace.update(cx, |workspace, cx| {
        workspace.settings = saved;
        cx.notify();
    });
}

fn saved_choice(fixture: &Fixture) -> ProjectCodexAccount {
    fixture
        .store
        .snapshot()
        .unwrap()
        .project(&fixture.project_id)
        .unwrap()
        .codex_account
        .clone()
}

fn in_memory_choice(fixture: &Fixture, cx: &mut gpui::TestAppContext) -> ProjectCodexAccount {
    fixture.workspace.read_with(cx, |workspace, _| {
        workspace
            .state
            .project(&fixture.project_id)
            .unwrap()
            .codex_account
            .clone()
    })
}

/// The status bar items' places, left to right, as drawn.
fn item_bounds(window: &Window, ids: &[&'static str]) -> Vec<(String, Bounds<Pixels>)> {
    ids.iter()
        .map(|id| ((*id).to_owned(), window.find(*id).bounds()))
        .collect()
}

fn assert_apart(items: &[(String, Bounds<Pixels>)]) {
    for pair in items.windows(2) {
        let ((left, a), (right, b)) = (&pair[0], &pair[1]);
        assert!(
            a.right() <= b.left() + px(0.5),
            "{left} {a:?} runs into {right} {b:?}"
        );
    }
}

#[gpui::test]
fn the_usage_item_reads_out_the_current_chat_or_every_account_without_covering_its_neighbours(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = window(cx, None, UsageMode::CurrentChat);
    let workspace = fixture.workspace.clone();
    // Nothing of Codex focused: the project's default, which inherits the system default.
    crate::form_input::test_turn(cx, fixture.handle, |window, _| {
        assert_eq!(
            window.find("usage-chip").label(),
            Some("Usage CODEX · System default · 5h 90% left / 7d 70% left")
        );
    });

    // All accounts: every available account, the failed one kept and marked, none for Grok.
    change_settings(cx, &fixture, |settings| {
        settings.status_bar.usage_mode = UsageMode::AllAccounts
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, _| {
        assert_eq!(
            window.find("usage-chip").label(),
            Some(
                format!(
                    "Usage CODEX · System default · 5h 90% / 7d 70% left; CODEX · {LONG_A} · 5h 80% / 7d 60% left; CODEX · {TEAM_B} · UNAVAILABLE"
                )
                .as_str()
            )
        );
        let ids = [
            "usage-chip",
            "status-codex-account",
            "top-orchestrator",
            "status-layout",
        ];
        assert_apart(&item_bounds(window, &ids));
    });

    // Narrower windows: labels, then figures, end in an ellipsis inside the item; the items
    // keep apart, inside the window, and the usage item keeps its least width.
    for width in [900.0, 560.0] {
        cx.simulate_window_resize(fixture.handle, gpui::size(px(width), px(480.)));
        crate::form_input::test_turn(cx, fixture.handle, |window, _| {
            assert_eq!(window.viewport_size().width, px(width));
            let items = item_bounds(
                window,
                &[
                    "usage-chip",
                    "status-codex-account",
                    "top-orchestrator",
                    "status-layout",
                ],
            );
            assert_apart(&items);
            for (id, bounds) in &items {
                assert!(
                    bounds.right() <= window.viewport_size().width,
                    "{id} {bounds:?} leaves the window"
                );
            }
            assert!(items[0].1.size.width >= ui_text::space(48.0) - px(0.5));
        });
    }

    // A click still opens the Usage panel for the details.
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("usage-chip", cx)
    });
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(
            workspace.panes[&workspace.active_pane].tabs
                [workspace.panes[&workspace.active_pane].active]
                .panel(),
            Some(PanelKind::Usage)
        );
    });
}

#[gpui::test]
fn the_focused_chat_or_shell_names_its_own_account(cx: &mut gpui::TestAppContext) {
    let fixture = window(cx, None, UsageMode::CurrentChat);
    let list = fixture.list.clone();
    let a = profile(&list, "account-a");
    let system = profile(&list, SYSTEM_DEFAULT_ID);
    let b = profile(&list, "account-b");
    // The views are never drawn: a drawn chat would ask its host.
    crate::form_input::test_turn(cx, fixture.handle, |window, app| {
        fixture.workspace.update(app, |workspace, cx| {
            let config = workspace.chat_config();
            let mut focus_on = |workspace: &mut Workspace,
                                info: Option<ChatInfo>,
                                listed: Option<ChatInfo>,
                                cx: &mut Context<Workspace>| {
                let id = info
                    .as_ref()
                    .or(listed.as_ref())
                    .map(|info| info.id.clone())
                    .unwrap();
                let view = cx.new(|cx| {
                    let mut view =
                        ChatView::open_deferred(id.clone(), None, config.clone(), window, cx);
                    if let Some(info) = info {
                        view.adopt_info_for_tests(info);
                    }
                    view
                });
                workspace.shared_chats = Some(listed.into_iter().collect());
                let pane = workspace.panes.get_mut(&2).unwrap();
                pane.tabs.push(Tab {
                    id: 90 + pane.tabs.len() as TabId,
                    title: "Chat".into(),
                    content: TabContent::Chat { chat_id: id, view },
                    hidden_since: None,
                });
                pane.active = pane.tabs.len() - 1;
                workspace.active_pane = 2;
                workspace.usage_focus(cx)
            };
            // The chat's own copy names its account.
            let codex = chat_info(
                serde_json::json!({"provider": "codex", "codex_account_id": "account-a"}),
            );
            assert_eq!(
                focus_on(workspace, Some(codex), None, cx),
                UsageFocus::Codex {
                    account: Selection::Account(a.clone()),
                    number: None,
                }
            );
            // Before its host answers, the listed chat does; no account is the system default.
            let listed = chat_info(serde_json::json!({"provider": "codex"}));
            assert_eq!(
                focus_on(workspace, None, Some(listed), cx),
                UsageFocus::Codex {
                    account: Selection::Account(system.clone()),
                    number: None,
                }
            );
            // An account the list does not have is not passed off as another.
            let gone =
                chat_info(serde_json::json!({"provider": "codex", "codex_account_id": "gone"}));
            assert_eq!(
                focus_on(workspace, Some(gone), None, cx),
                UsageFocus::Codex {
                    account: Selection::Unavailable,
                    number: None,
                }
            );
            // Claude and Muse chats fall back to the project's default, as before.
            for provider in ["claude", "muse"] {
                let other = chat_info(serde_json::json!({"provider": provider}));
                assert_eq!(
                    focus_on(workspace, Some(other), None, cx),
                    UsageFocus::Other
                );
            }

            // An orchestrator chat is found among the orchestrators the host listed.
            let orchestrator = chat_info(
                serde_json::json!({"provider": "codex", "codex_account_id": "account-b"}),
            );
            workspace.shared_chats = None;
            workspace.chat_orchestrators = Some(vec![orchestrator.clone()]);
            let view = cx.new(|cx| {
                ChatView::open_deferred(orchestrator.id.clone(), None, config.clone(), window, cx)
            });
            let pane = workspace.panes.get_mut(&2).unwrap();
            pane.tabs.push(Tab {
                id: 110 + pane.tabs.len() as TabId,
                title: "P·ORCH".into(),
                content: TabContent::Chat {
                    chat_id: orchestrator.id.clone(),
                    view,
                },
                hidden_since: None,
            });
            pane.active = pane.tabs.len() - 1;
            assert_eq!(
                workspace.usage_focus(cx),
                UsageFocus::Codex {
                    account: Selection::Account(b.clone()),
                    number: None,
                }
            );
            workspace.chat_orchestrators = None;

            // A Codex shell by its frozen home, numbered as its tab is beside another account's
            // shell; a Muse shell falls back.
            let mut codex_shell = shell("codex-shell", ShellKind::Project, Some("p"));
            codex_shell.codex_home = Some(b.home.clone());
            let mut other_codex = shell("other-codex", ShellKind::Project, Some("p"));
            other_codex.codex_home = Some(a.home.clone());
            let mut muse_shell = shell("muse-shell", ShellKind::Project, Some("p"));
            muse_shell.harness = Some(HarnessKind::Muse);
            let mut grok_shell = shell("grok-shell", ShellKind::Project, Some("p"));
            grok_shell.harness = Some(HarnessKind::Grok);
            workspace.shells = vec![codex_shell, other_codex, muse_shell, grok_shell];
            for (shell_id, expected) in [
                (
                    "codex-shell",
                    UsageFocus::Codex {
                        account: Selection::Account(b.clone()),
                        number: Some(2),
                    },
                ),
                ("muse-shell", UsageFocus::Other),
                (
                    "grok-shell",
                    UsageFocus::GrokShell {
                        id: "grok-shell".into(),
                        alive: true,
                    },
                ),
            ] {
                let pane = workspace.panes.get_mut(&2).unwrap();
                pane.tabs.push(Tab {
                    id: 120 + pane.tabs.len() as TabId,
                    title: "Shell".into(),
                    content: TabContent::Shell {
                        shell_id: shell_id.into(),
                        worktree_id: None,
                        terminal: None,
                        attach_error: Some("not attached in a test".into()),
                        attach_failures: u8::MAX,
                    },
                    hidden_since: None,
                });
                pane.active = pane.tabs.len() - 1;
                assert_eq!(workspace.usage_focus(cx), expected);
            }
            // The focused Grok session's figures are read only while Current chat shows them.
            assert_eq!(workspace.grok_usage_targets(), ["grok-shell"]);
            workspace.settings.status_bar.usage_mode = UsageMode::AllAccounts;
            assert!(workspace.grok_usage_targets().is_empty());
            workspace.settings.status_bar.usage_mode = UsageMode::CurrentChat;

            // Leave nothing to draw.
            workspace.panes.get_mut(&2).unwrap().tabs.truncate(1);
            workspace.panes.get_mut(&2).unwrap().active = 0;
            workspace.shells.clear();
            workspace.shared_chats = None;
            workspace.active_pane = 1;
        });
    });
}

#[gpui::test]
fn the_account_item_opens_a_menu_that_saves_the_projects_default_for_new_codex_sessions(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = window(cx, None, UsageMode::CurrentChat);
    let workspace = fixture.workspace.clone();
    // An open Project Settings follows a choice made here.
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.ensure_project_settings(window, cx)
        });
        assert_eq!(
            window.find("status-codex-account").label(),
            Some("New Codex sessions: DEFAULT (APP) · System default · sys@example.test · plus")
        );
        window.click("status-codex-account", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(workspace.read(cx).account_menu_open);
        let checked = |id: &'static str| window.find(id).checked();
        let inherit = window.find("account-menu-inherit");
        assert_eq!(inherit.role(), Some(gpui::Role::MenuItemRadio));
        assert_eq!(
            inherit.label(),
            Some("Inherit app default, Uses System default · sys@example.test · plus")
        );
        assert_eq!(checked("account-menu-inherit"), Some(true));
        assert_eq!(
            window.find("account-menu-system").label(),
            Some("System default, sys@example.test · plus")
        );
        assert_eq!(checked("account-menu-system"), Some(false));
        assert_eq!(
            window.find("account-menu-saved-account-a").label(),
            Some(LONG_A)
        );
        // An unavailable account cannot be chosen anew.
        window.click("account-menu-saved-account-c", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(workspace.read(cx).account_menu_open);
        assert_eq!(workspace.read(cx).notice, None);
        window.press("escape", cx);
    });
    assert_eq!(saved_choice(&fixture), ProjectCodexAccount::Inherit);
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(!workspace.read(cx).account_menu_open, "Escape closes it");
        window.click("status-codex-account", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(workspace.read(cx).account_menu_open);
        // A click elsewhere closes it.
        window.click("top-orchestrator", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(!workspace.read(cx).account_menu_open);
        // That click opened the global menu; the account item takes over from it.
        window.click("status-codex-account", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(workspace.read(cx).account_menu_open);
        assert!(!workspace.read(cx).global_menu_open);
        window.click("account-menu-system", cx);
    });

    // Saved through the store as Project Settings saves it, and shown at once everywhere.
    assert_eq!(saved_choice(&fixture), ProjectCodexAccount::SystemDefault);
    assert_eq!(
        in_memory_choice(&fixture, cx),
        ProjectCodexAccount::SystemDefault
    );
    workspace.read_with(cx, |workspace, cx| {
        assert!(!workspace.account_menu_open);
        assert_eq!(workspace.notice.as_deref(), Some("Saved Fixture"));
        let panel = workspace.project_settings_panel.as_ref().unwrap();
        assert_eq!(
            panel.read(cx).codex_account_for_tests(),
            &ProjectCodexAccount::SystemDefault
        );
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert_eq!(
            window.find("status-codex-account").label(),
            Some("New Codex sessions: DEFAULT (SYSTEM) · System default · sys@example.test · plus")
        );
        window.click("status-codex-account", cx);
    });

    // A choice the store refuses (no saved account metadata here) changes nothing, and says
    // why.
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert_eq!(window.find("account-menu-system").checked(), Some(true));
        window.click("account-menu-saved-account-a", cx);
    });
    assert_eq!(saved_choice(&fixture), ProjectCodexAccount::SystemDefault);
    assert_eq!(
        in_memory_choice(&fixture, cx),
        ProjectCodexAccount::SystemDefault
    );
    workspace.read_with(cx, |workspace, _| {
        let notice = workspace.notice.as_deref().unwrap_or_default();
        assert!(notice.contains("Codex account"), "{notice}");
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, _| {
        assert_eq!(
            window.find("status-codex-account").label(),
            Some("New Codex sessions: DEFAULT (SYSTEM) · System default · sys@example.test · plus")
        );
    });

    // Inherit goes back to following the app.
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("status-codex-account", cx)
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("account-menu-inherit", cx)
    });
    assert_eq!(saved_choice(&fixture), ProjectCodexAccount::Inherit);
    // The refusal's message does not outlive a choice that saved.
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.notice.as_deref(), Some("Saved Fixture"));
    });

    // Opening a pane's menu, or focus mode, closes it, as they close the other status menus.
    for close in [
        (|workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>| {
            workspace.toggle_panel_menu(2, window, cx)
        }) as fn(&mut Workspace, &mut Window, &mut Context<Workspace>),
        |workspace, window, cx| workspace.set_focus_mode(true, window, cx),
    ] {
        crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
            window.click("status-codex-account", cx)
        });
        crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
            assert!(workspace.read(cx).account_menu_open);
            workspace.update(cx, |workspace, cx| close(workspace, window, cx));
            assert!(!workspace.read(cx).account_menu_open);
            workspace.update(cx, |workspace, cx| {
                workspace.panel_menu = None;
                workspace.set_focus_mode(false, window, cx);
            });
        });
    }

    // The secondary row still leads to Project Settings.
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("status-codex-account", cx)
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("account-menu-project-settings", cx)
    });
    workspace.read_with(cx, |workspace, _| {
        assert!(!workspace.account_menu_open);
        assert!(
            panel_tab_in(&workspace.panes, PanelKind::ProjectSettings).is_some(),
            "Project Settings is open"
        );
    });
}

#[gpui::test]
fn the_account_menu_follows_its_item_and_waits_honestly_for_the_account_list(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = window(cx, None, UsageMode::CurrentChat);
    let workspace = fixture.workspace.clone();
    // The menu opens over its item, inside the window; moved to the other side while it is
    // open, the item takes it along.
    let over_item = |window: &Window| {
        let item = window.find("status-codex-account").bounds();
        let menu = window.find("account-menu-inherit").bounds();
        assert!(menu.left() >= px(0.) && menu.right() <= window.viewport_size().width);
        assert!(
            menu.left() < item.right() && item.left() < menu.right() + px(8.),
            "menu {menu:?} is not over item {item:?}"
        );
        assert!(menu.bottom() <= item.top());
        item
    };
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("status-codex-account", cx)
    });
    let right = std::cell::Cell::new(px(0.));
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(workspace.read(cx).account_menu_open);
        right.set(over_item(window).left());
    });
    change_settings(cx, &fixture, |settings| {
        settings.status_bar.set_side(
            status_bar::StatusItemKind::CodexAccount,
            status_bar::StatusSide::Left,
        )
    });
    // One frame draws the item in its new place, the next hangs the menu from it.
    crate::form_input::test_turn(cx, fixture.handle, |_, _| {});
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(workspace.read(cx).account_menu_open);
        assert!(
            over_item(window).left() < right.get(),
            "the item moved left"
        );
    });

    // Hidden, it takes its menu with it.
    change_settings(cx, &fixture, |settings| {
        settings
            .status_bar
            .set_visible(status_bar::StatusItemKind::CodexAccount, false)
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(!workspace.read(cx).account_menu_open);
        assert!(window.try_find("account-menu-inherit").is_none());
    });

    // No account list yet (and one already being read): no saved account is offered.
    change_settings(cx, &fixture, |settings| {
        settings
            .status_bar
            .set_visible(status_bar::StatusItemKind::CodexAccount, true)
    });
    cx.update(|cx| {
        cx.set_global(settings::CodexAccountsState {
            snapshot: None,
            pending: true,
        })
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert_eq!(
            window.find("status-codex-account").label(),
            Some("New Codex sessions: DEFAULT (APP) · System default · email unknown")
        );
        window.click("status-codex-account", cx)
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(workspace.read(cx).account_menu_open);
        assert!(window.try_find("account-menu-inherit").is_some());
        assert!(window.try_find("account-menu-system").is_some());
        assert!(window.try_find("account-menu-saved-account-a").is_none());
        assert!(cx.global::<settings::CodexAccountsState>().pending);
    });
}

/// With saved account metadata in a private Orca profile, a saved account chosen from the
/// menu is what every new Codex session of the project now resolves (chats, shells and the
/// orchestrator's workers alike), while a running session keeps its own and a named account
/// still wins. Runs in a child process: the profile is chosen by environment.
#[gpui::test]
fn a_saved_account_chosen_in_the_menu_is_what_new_codex_sessions_of_the_project_resolve(
    cx: &mut gpui::TestAppContext,
) {
    const CHILD: &str = "RIWORK_STATUS_ACCOUNT_CHILD";
    let Some(root) = std::env::var_os(CHILD).map(PathBuf::from) else {
        let root = std::env::temp_dir().join(format!("rw-account-child-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("home")).unwrap();
        // Orca's CLI, as far as this test knows: one that always fails.
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(root.join("orca-fails"), "#!/bin/sh\nexit 1\n").unwrap();
            std::fs::set_permissions(
                root.join("orca-fails"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "account_status::tests::a_saved_account_chosen_in_the_menu_is_what_new_codex_sessions_of_the_project_resolve",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, &root)
            .env("ORCA_USER_DATA_PATH", root.join("profile"))
            .env("ORCA_CLI_COMMAND", root.join("orca-fails"))
            .env("RIWORK_HOME", root.join("state"))
            .env("TMUX_TMPDIR", root.join("tmux"))
            .env("HOME", root.join("home"))
            .env_remove("CODEX_HOME")
            .env_remove("RIWORK_CODEX_ACCOUNT_HOME")
            .env_remove("RIWORK_SHELL_ID")
            .env_remove("RIWORK_CHAT_ID")
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    use std::os::unix::fs::PermissionsExt;
    let orca_profile = root.join("profile");
    for id in ["account-a", "account-b"] {
        std::fs::create_dir_all(orca_profile.join("codex-accounts").join(id).join("home")).unwrap();
    }
    // The list as it is saved: the window reads it from its own data directory below.
    let write_cache = |home: &Path| {
        let cache = home.join("codex-accounts.json");
        std::fs::write(
            &cache,
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "user_data": orca_profile,
                "accounts": [
                    {"id": "account-a", "email": "a@example.test", "workspaceLabel": "Team A", "managedHomeRuntime": "host"},
                    {"id": "account-b", "email": "b@example.test", "workspaceLabel": "Team B", "managedHomeRuntime": "host"}
                ],
                "source_active_id": "account-a"
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    let scratch = root.join("list");
    std::fs::create_dir_all(&scratch).unwrap();
    write_cache(&scratch);
    let list = AccountsSnapshot {
        accounts: codex_accounts::cached_accounts(&scratch).unwrap(),
        ..Default::default()
    };
    let fixture = window(cx, Some(list.clone()), UsageMode::CurrentChat);
    write_cache(&fixture.home);
    let workspace = fixture.workspace.clone();
    let a_home = profile(&list, "account-a").home;
    let b_home = profile(&list, "account-b").home;

    // A Codex session already running under account B.
    let mut running = shell("running", ShellKind::Project, Some(&fixture.project_id));
    running.codex_home = Some(b_home.clone());
    running.codex_account_id = Some("account-b".into());
    workspace.update(cx, |workspace, _| workspace.shells = vec![running.clone()]);

    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("status-codex-account", cx)
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert_eq!(
            window.find("account-menu-saved-account-a").label(),
            Some("a@example.test · Team A")
        );
        window.click("account-menu-saved-account-a", cx);
    });
    assert_eq!(
        saved_choice(&fixture),
        ProjectCodexAccount::Saved("account-a".into())
    );
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.notice.as_deref(), Some("Saved Fixture"));
        assert_eq!(
            workspace.project_codex_selection(cx),
            Selection::Account(profile(&list, "account-a"))
        );
        // The running session keeps its account.
        assert_eq!(workspace.shells[0].codex_home.as_ref(), Some(&b_home));
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, _| {
        assert_eq!(
            window.find("status-codex-account").label(),
            Some("New Codex sessions: DEFAULT (PROJECT) · a@example.test")
        );
    });

    // What a new chat, shell or orchestrator worker of the project launches with when no
    // account is named (`selected_codex_binding`), and what naming one still gives.
    let binding =
        codex_accounts::resolve_project_launch_binding(&fixture.home, &fixture.project_id).unwrap();
    assert_eq!(binding.id.as_deref(), Some("account-a"));
    assert_eq!(binding.home, a_home);
    let named = codex_accounts::resolve_account(&fixture.home, "Team B").unwrap();
    assert_eq!(named.id.as_deref(), Some("account-b"));
    assert_eq!(named.home, b_home);

    // The bar names the account a launch resolves, for every choice and app selection.
    for choice in [
        ProjectCodexAccount::Inherit,
        ProjectCodexAccount::SystemDefault,
        ProjectCodexAccount::Saved("account-a".into()),
        ProjectCodexAccount::Saved("account-b".into()),
    ] {
        for app in [None, Some("account-a"), Some("account-b")] {
            let launched =
                codex_accounts::resolve_project_choice_binding(&fixture.home, &choice, app)
                    .unwrap();
            assert_eq!(
                project_selection(&choice, app, Some(&list)),
                Selection::Account(CodexProfile {
                    home: launched.home.clone(),
                    label: launched.label.unwrap_or_else(|| SYSTEM_LABEL.to_owned()),
                }),
                "{choice:?} with the app on {app:?}"
            );
        }
    }

    // With no account list yet, opening the menu asks for one, as Settings does. Orca's CLI
    // here is a stand-in that fails, so the list comes back empty and says why.
    cx.update(|cx| {
        cx.set_global(settings::CodexAccountsState {
            snapshot: None,
            pending: false,
        })
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("status-codex-account", cx)
    });
    cx.run_until_parked();
    cx.read(|cx| {
        let accounts = cx.global::<settings::CodexAccountsState>();
        assert!(!accounts.pending);
        let read = accounts
            .snapshot
            .as_ref()
            .expect("the menu asked for the list");
        assert!(read.error.is_some());
        assert!(
            read.accounts
                .iter()
                .all(|account| account.is_system_default)
        );
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, _| {
        assert!(window.try_find("account-menu-saved-account-b").is_none());
        // The saved choice the failed list cannot show stays marked, and cannot be chosen.
        assert_eq!(
            window.find("account-menu-saved-account-a").checked(),
            Some(true)
        );
    });
    assert_eq!(
        saved_choice(&fixture),
        ProjectCodexAccount::Saved("account-a".into())
    );
}
