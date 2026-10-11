use super::*;
use gpui_kit::test::TestWindowExt;

#[test]
fn a_new_dashboard_goes_to_the_work_area_not_the_locked_navigation() {
    let panes = [1, 2, 3];
    let locked = |pane: PaneId| pane == 1;
    // The main pane takes it.
    assert_eq!(dashboard_pane(panes, Some(3), 1, &locked), 3);
    // Without one, the selected pane, unless it is the locked navigation pane.
    assert_eq!(dashboard_pane(panes, None, 2, &locked), 2);
    assert_eq!(dashboard_pane(panes, None, 1, &locked), 2);
    // A locked main pane is passed over like any locked pane.
    assert_eq!(dashboard_pane(panes, Some(1), 1, &locked), 2);
    // Everything locked: the selected pane, rather than nowhere.
    assert_eq!(dashboard_pane(panes, None, 3, &|_| true), 3);
}

fn panel(id: TabId, kind: PanelKind) -> Tab {
    new_panel_tab(id, Workspace::panel_title(kind), kind)
}

struct Fixture {
    handle: gpui::AnyWindowHandle,
    workspace: Entity<Workspace>,
    home: PathBuf,
    project_id: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// A local project's window with a locked navigation pane (1, focused) beside a work pane
/// (2), in a private data directory that has no sessions, chats or chat host. Its layout is
/// laid out as if loaded, so nothing asks tmux.
fn window(cx: &mut gpui::TestAppContext) -> Fixture {
    cx.update(|cx| {
        cx.set_global(Settings::default());
        cx.set_global(Appearance::resolve(theme::ThemeChoice::Native, false));
        cx.set_global(settings::CodexAccountsState::default());
        cx.set_global(RemoteState::default());
        cx.set_global(AccountUsage::default());
        cx.set_global(CuaSetupState::default());
        ui_text::init(cx);
        tooltip::init(cx);
    });
    let home = std::env::temp_dir().join(format!("rw-global-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let project: Project = serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "name": "Fixture",
        "root": home,
        "created_at": 1
    }))
    .unwrap();
    let project_id = project.id.clone();
    let startup = WorkspaceStartup {
        store: Store::open(&home).unwrap(),
        layouts: LayoutStore::open(&home).unwrap(),
        settings_store: SettingsStore::open(&home).unwrap(),
        sessions: SessionManager::at(home.clone()).unwrap(),
        state: State {
            projects: vec![project.clone()],
            ..Default::default()
        },
        project,
        remote: None,
    };
    let (handle, workspace) = crate::form_input::test_window(cx, move |window, cx| {
        Workspace::build(startup, None, window, cx, false)
    });
    crate::form_input::test_turn(cx, handle, |_, cx| {
        workspace.update(cx, |workspace, _| {
            workspace.panes = BTreeMap::from([
                (
                    1,
                    Pane {
                        tabs: vec![panel(1, PanelKind::Projects)],
                        active: 0,
                    },
                ),
                (
                    2,
                    Pane {
                        tabs: vec![panel(2, PanelKind::Usage)],
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
    Fixture {
        handle,
        workspace,
        home,
        project_id,
    }
}

/// Every dashboard tab of the window: (pane, tab, selected).
fn dashboards(workspace: &Workspace) -> Vec<(PaneId, TabId, bool)> {
    workspace
        .panes
        .iter()
        .flat_map(|(pane_id, pane)| {
            pane.tabs
                .iter()
                .enumerate()
                .filter(|(_, tab)| tab.panel() == Some(PanelKind::Dashboard))
                .map(move |(index, tab)| (*pane_id, tab.id, index == pane.active))
        })
        .collect()
}

/// Opening, drawing and refreshing the dashboard made no chat, no orchestrator, no session.
fn nothing_was_started(fixture: &Fixture, cx: &mut gpui::TestAppContext) {
    fixture.workspace.read_with(cx, |workspace, _| {
        assert!(workspace.orchestrators_starting.is_empty());
        assert!(
            workspace
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .all(|tab| tab.chat().is_none()),
            "no chat tab was opened"
        );
    });
    assert!(!fixture.home.join("chats").exists(), "no chat was made");
    assert!(
        !fixture.home.join("orchestrator").exists(),
        "no orchestrator folder was made"
    );
    assert!(
        !fixture.home.join("sessions.json").exists(),
        "no session was registered"
    );
}

#[gpui::test]
fn the_dashboard_opens_once_in_the_work_area_and_is_saved_with_the_layout(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = window(cx);
    let workspace = fixture.workspace.clone();
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        workspace.update(cx, |workspace, cx| workspace.open_dashboard(window, cx));
    });
    cx.run_until_parked();
    let first = workspace.read_with(cx, |workspace, _| {
        let open = dashboards(workspace);
        assert_eq!(open.len(), 1);
        let (pane, tab, selected) = open[0];
        // The work pane takes it, not the locked navigation pane that had focus.
        assert_eq!((pane, selected), (2, true));
        assert_eq!(workspace.active_pane, 2);
        assert!(workspace.dashboard_on_screen());
        // The automations were read off the window: an empty ledger, not an error.
        assert_eq!(workspace.dashboard.schedules, Some(Ok(Vec::new())));
        tab
    });

    // Something else comes to the front; opening it again brings the same tab back.
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.select_tab(2, 2, window, cx);
            assert!(!workspace.dashboard_on_screen());
            workspace.open_dashboard(window, cx);
            workspace.open_dashboard(window, cx);
        });
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(dashboards(workspace), [(2, first, true)]);
    });

    // The tab is part of the project's saved layout, selected, and a window that loads the
    // layout again gets it back.
    let saved = workspace.read_with(cx, |workspace, _| {
        workspace
            .layouts
            .load(&fixture.project_id)
            .unwrap()
            .unwrap()
    });
    assert_eq!(
        saved.panes[&2].active_tab_key.as_deref(),
        Some("panel:dashboard")
    );
    assert!(saved.panes[&2].tabs.contains(&SavedTab::Panel {
        panel: PanelKind::Dashboard
    }));
    nothing_was_started(&fixture, cx);
}

#[gpui::test]
fn the_global_item_opens_a_menu_whose_dashboard_row_opens_the_dashboard(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = window(cx);
    let workspace = fixture.workspace.clone();
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("top-orchestrator", cx)
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(workspace.read(cx).global_menu_open);
        // The menu says what each row does before anything is clicked.
        let chat = window.find("global-menu-chat");
        assert_eq!(chat.role(), Some(gpui::Role::MenuItem));
        // Nothing has been read yet, so it opens or starts as Settings say, and says so.
        assert_eq!(chat.label(), Some("Open global chat: Reading chats…"));
        let dashboard = window.find("global-menu-dashboard");
        assert_eq!(dashboard.label(), Some("Dashboard: Reading sessions…"));
        window.press("escape", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(!workspace.read(cx).global_menu_open, "Escape closes it");
        window.click("top-orchestrator", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(workspace.read(cx).global_menu_open);
        window.click("global-menu-dashboard", cx);
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, _| {
        assert!(!workspace.global_menu_open);
        assert_eq!(dashboards(workspace).len(), 1);
        assert!(workspace.dashboard_on_screen());
    });
    nothing_was_started(&fixture, cx);
}

#[gpui::test]
fn a_dashboard_row_opens_the_global_chat_there_is_and_reuses_its_tab(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = window(cx);
    let workspace = fixture.workspace.clone();
    let chat: ChatInfo = serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "provider": "claude",
        "cwd": fixture.home.join("orchestrator"),
        "title": "Global orchestrator",
        "created_at_unix": 1,
        "state": {"state": "waiting"},
        "orchestrator": {"scope": "global"}
    }))
    .unwrap();
    // A project's orchestrator chat beside it must not be taken for the global one.
    let project_chat: ChatInfo = serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "provider": "codex",
        "project_id": fixture.project_id,
        "cwd": "/tmp",
        "title": "Project orchestrator",
        "created_at_unix": 2,
        "state": {"state": "idle"},
        "orchestrator": {"scope": "project", "project_id": fixture.project_id}
    }))
    .unwrap();
    let target = Target::Chat {
        id: chat.id.clone(),
        project_id: None,
        orchestrator: Some(OrchestratorScope::Global),
    };
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.shared_chats = Some(vec![project_chat.clone(), chat.clone()]);
            workspace.chat_orchestrators = Some(vec![project_chat.clone(), chat.clone()]);
            let overview = workspace.dashboard_overview();
            assert_eq!(
                overview.global,
                global_overview::GlobalOrchestrator::Chat {
                    id: chat.id.clone(),
                    provider: Provider::Claude,
                    state: chat::model::ChatState::Waiting,
                }
            );
            // It waits for an answer, so the dashboard lists it with the row that opens it.
            assert_eq!(overview.attention[0].target, target);
            workspace.dashboard_action(DashboardAction::Open(target.clone()), window, cx);
        });
    });
    cx.run_until_parked();
    let chat_tab = |workspace: &Workspace| {
        workspace
            .panes
            .iter()
            .flat_map(|(pane, tabs)| tabs.tabs.iter().map(move |tab| (*pane, tab)))
            .filter(|(_, tab)| tab.chat_id() == Some(chat.id.as_str()))
            .map(|(pane, tab)| (pane, tab.id, tab.title.clone()))
            .collect::<Vec<_>>()
    };
    let opened = workspace.read_with(cx, |workspace, _| {
        let tabs = chat_tab(workspace);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].2, "G·ORCH · GLOBAL");
        // It opened the chat there is: nothing was asked to find or make an orchestrator.
        assert!(workspace.orchestrators_starting.is_empty());
        assert!(workspace.global_chat_on_screen());
        tabs[0].clone()
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.select_tab(2, 2, window, cx);
            workspace.dashboard_action(DashboardAction::Open(target.clone()), window, cx);
        });
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(
            chat_tab(workspace),
            [opened.clone()],
            "the same tab is reused"
        );
        assert!(workspace.global_chat_on_screen());
        assert!(
            workspace
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .all(|tab| tab.chat_id() != Some(project_chat.id.as_str())),
            "the project's orchestrator stays where it was"
        );
    });
}

#[gpui::test]
fn a_chat_the_window_no_longer_knows_is_reported_gone_not_made(cx: &mut gpui::TestAppContext) {
    let fixture = window(cx);
    let workspace = fixture.workspace.clone();
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.shared_chats = Some(Vec::new());
            workspace.dashboard_action(
                DashboardAction::Open(Target::Chat {
                    id: uuid::Uuid::new_v4().to_string(),
                    project_id: None,
                    orchestrator: Some(OrchestratorScope::Global),
                }),
                window,
                cx,
            );
            assert_eq!(workspace.notice.as_deref(), Some("That chat is gone."));
        });
    });
    cx.run_until_parked();
    nothing_was_started(&fixture, cx);
}
