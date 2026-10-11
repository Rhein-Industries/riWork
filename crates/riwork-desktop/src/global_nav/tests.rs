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

/// The window works out each chat tab's image scope from the chat's identity and the store,
/// and keeps it in step: here an ordinary chat of the window's project and the global chat.
#[gpui::test]
fn chat_tabs_get_image_scopes_from_their_identity_and_follow_the_store(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = window(cx);
    let workspace = fixture.workspace.clone();
    // The data directory as the window and the chat host spell it (resolved, as on macOS
    // `/var` is a link): orchestrator chats are made to work in it.
    let home = workspace.read_with(cx, |workspace, _| {
        workspace.sessions.state_home().to_path_buf()
    });
    let project = fixture.project_id.clone();
    let other = uuid::Uuid::new_v4().to_string();
    let work = home.join("work");
    let (alpha, feature, beta) = (
        work.join("alpha"),
        work.join("alpha-feature"),
        work.join("beta"),
    );
    let chat = |id: &str, value: serde_json::Value| -> ChatInfo {
        let mut base = serde_json::json!({
            "id": id,
            "provider": "codex",
            "title": "Chat",
            "created_at_unix": 1,
        });
        for (key, field) in value.as_object().unwrap() {
            base[key] = field.clone();
        }
        serde_json::from_value(base).unwrap()
    };
    let ordinary = chat(
        &uuid::Uuid::new_v4().to_string(),
        serde_json::json!({"project_id": project, "worktree_id": "wt-feature", "cwd": feature}),
    );
    let global = chat(
        &uuid::Uuid::new_v4().to_string(),
        serde_json::json!({"cwd": home.join("orchestrator"), "orchestrator": {"scope": "global"}}),
    );
    let tab_of = |workspace: &Workspace, id: &str| {
        workspace
            .panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .find(|tab| tab.chat_id() == Some(id))
            .and_then(Tab::chat)
            .cloned()
            .unwrap()
    };
    let (ordinary_tab, global_tab) = {
        let (ordinary, global) = (ordinary.clone(), global.clone());
        let (project, other) = (project.clone(), other.clone());
        let (alpha, feature, beta) = (alpha.clone(), feature.clone(), beta.clone());
        let home = home.clone();
        let mut tabs = None;
        crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
            workspace.update(cx, |workspace, cx| {
                let mut state = workspace.state.clone();
                state.projects[0].root = alpha.clone();
                let mut second = state.projects[0].clone();
                second.id = other.clone();
                second.root = beta.clone();
                state.projects.push(second);
                state.worktrees = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "wt-main", "project_id": project, "branch": "main",
                        "path": alpha, "is_primary": true, "created_at": 1
                    }))
                    .unwrap(),
                    serde_json::from_value(serde_json::json!({
                        "id": "wt-feature", "project_id": project, "branch": "feature",
                        "path": feature, "created_at": 1
                    }))
                    .unwrap(),
                ];
                workspace.state = state;
                workspace.shared_chats = Some(vec![ordinary.clone(), global.clone()]);
                // A project chat's tab is recorded in the project's shared tab store.
                project_tabs::TabStore::at(&home, &project)
                    .unwrap()
                    .reconcile(&[project_tabs::Session::chat(&ordinary)])
                    .unwrap();
                workspace.shared_tabs = Some(Vec::new());
                workspace.show_existing_chat(ordinary.clone(), window, cx);
                workspace.show_orchestrator_chat(global.clone(), window, cx);
                assert_eq!(workspace.notice, None);
                let ordinary_tab = tab_of(workspace, &ordinary.id);
                let global_tab = tab_of(workspace, &global.id);
                ordinary_tab.update(cx, |view, _| view.adopt_info_for_tests(ordinary.clone()));
                global_tab.update(cx, |view, _| view.adopt_info_for_tests(global.clone()));
                workspace.refresh_chat_image_scopes(cx);
                tabs = Some((ordinary_tab, global_tab));
            });
        });
        tabs.unwrap()
    };
    let roots = |view: &Entity<ChatView>, cx: &mut gpui::TestAppContext| {
        view.read_with(cx, |view, _| view.image_scope().roots().to_vec())
    };
    let mut own = vec![
        alpha.clone(),
        feature.clone(),
        home.join("orchestrators/projects").join(&project),
    ];
    own.sort();
    assert_eq!(roots(&ordinary_tab, cx), own);
    assert_eq!(
        ordinary_tab.read_with(cx, |view, _| view
            .image_scope()
            .cwd()
            .map(Path::to_path_buf)),
        Some(feature.clone())
    );
    let mut everything = vec![
        alpha.clone(),
        feature.clone(),
        beta.clone(),
        home.join("orchestrator"),
        home.join("orchestrators/projects").join(&project),
        home.join("orchestrators/projects").join(&other),
    ];
    everything.sort();
    assert_eq!(roots(&global_tab, cx), everything);

    // The store forgets the feature worktree; the next refresh takes it out of both scopes.
    crate::form_input::test_turn(cx, fixture.handle, |_, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace
                .state
                .worktrees
                .retain(|worktree| worktree.id != "wt-feature");
            workspace.refresh_chat_image_scopes(cx);
        });
    });
    assert!(!roots(&ordinary_tab, cx).contains(&feature));
    assert!(!roots(&global_tab, cx).contains(&feature));

    // The host lists the global chat as an ordinary one: no folders at all.
    let mut demoted = global.clone();
    demoted.orchestrator = None;
    crate::form_input::test_turn(cx, fixture.handle, |_, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.shared_chats = Some(vec![ordinary.clone(), demoted]);
            workspace.refresh_chat_image_scopes(cx);
        });
    });
    assert!(roots(&global_tab, cx).is_empty());
    // The global chat is still the one it was: same tab, same chat.
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(tab_of(workspace, &global.id), global_tab);
    });
}
