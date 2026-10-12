use super::*;
use crate::dashboard::DashboardAction;
use crate::global_window::tests::{
    GlobalFixture, chat_info, global_chat, open_global, sample, shell,
};
use global_overview::Target;
use gpui_kit::test::TestWindowExt;
use riwork_core::global_window::GlobalWindowState;

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

/// The global window beside the project window, over the same data directory.
fn global_beside(fixture: &Fixture, cx: &mut gpui::TestAppContext) -> GlobalFixture {
    open_global(cx, &fixture.home, GlobalWindowState::default())
}

fn chat_tabs_of(workspace: &Workspace) -> Vec<String> {
    workspace
        .panes
        .values()
        .flat_map(|pane| &pane.tabs)
        .filter_map(|tab| tab.chat_id().map(str::to_owned))
        .collect()
}

#[gpui::test]
fn the_global_item_opens_a_menu_whose_rows_bring_up_the_global_window(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = window(cx);
    let workspace = fixture.workspace.clone();
    let global = global_beside(&fixture, cx);
    let windows = cx.update(|cx| cx.windows().len());
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("top-orchestrator", cx)
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(workspace.read(cx).global_menu_open);
        // The menu says what each row does before anything is clicked.
        let chat = window.find("global-menu-chat");
        assert_eq!(chat.role(), Some(gpui::Role::MenuItem));
        assert_eq!(chat.label(), Some("Open global chat: Reading chats…"));
        let dashboard = window.find("global-menu-dashboard");
        assert_eq!(
            dashboard.label(),
            Some("Dashboard, shown: Reading sessions…")
        );
        window.press("escape", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(!workspace.read(cx).global_menu_open, "Escape closes it");
        window.click("top-orchestrator", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("global-menu-dashboard", cx);
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, cx| {
        assert!(!workspace.global_menu_open);
        assert!(dashboards(workspace).is_empty(), "no dashboard tab here");
        assert_eq!(global_window::shown_view(cx), Some(GlobalView::Dashboard));
    });

    // The chat row, with the global chat known: the global window shows it, and this window
    // gets no tab for it.
    let chat = global_chat(&fixture.home);
    sample(cx, &global, Vec::new(), vec![chat.clone()]);
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        workspace.update(cx, |workspace, _| {
            workspace.shared_chats = Some(vec![chat.clone()]);
            workspace.chat_orchestrators = Some(vec![chat.clone()]);
        });
        window.click("top-orchestrator", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert_eq!(
            window.find("global-menu-chat").label(),
            Some("Open global chat: Claude · Waiting")
        );
        window.click("global-menu-chat", cx);
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, cx| {
        assert!(chat_tabs_of(workspace).is_empty());
        assert_eq!(global_window::shown_view(cx), Some(GlobalView::Chat));
    });
    // The same global window, every time.
    assert_eq!(cx.update(|cx| cx.windows().len()), windows);
    nothing_was_started(&fixture, cx);
}

#[gpui::test]
fn a_project_window_hands_the_dashboard_and_the_global_orchestrator_to_the_global_window(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = window(cx);
    let workspace = fixture.workspace.clone();
    let global = global_beside(&fixture, cx);
    let chat = global_chat(&fixture.home);
    sample(cx, &global, Vec::new(), vec![chat.clone()]);
    // The pane menu's Dashboard, and anything that opens the dashboard panel.
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.open_panel(PanelKind::Dashboard, 2, window, cx)
        });
    });
    workspace.read_with(cx, |workspace, cx| {
        assert!(dashboards(workspace).is_empty());
        assert_eq!(global_window::shown_view(cx), Some(GlobalView::Dashboard));
    });
    // A route that would show the global chat here (a dashboard row of an older window, a
    // chat request, a notification) shows it in the global window instead.
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.shared_chats = Some(vec![chat.clone()]);
            workspace.show_orchestrator_chat(chat.clone(), window, cx);
            workspace.show_existing_chat(chat.clone(), window, cx);
        });
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, cx| {
        assert!(chat_tabs_of(workspace).is_empty());
        assert_eq!(global_window::shown_view(cx), Some(GlobalView::Chat));
        // Nothing was asked of the project's layout for it.
        assert!(
            workspace
                .layouts
                .chat_open_requests(&fixture.project_id)
                .unwrap()
                .is_empty()
        );
    });
    // The pane menu's Global orchestrator (an explicit Chat) and an automation of the global
    // orchestrator that asks for its chat: the global window again, and the chat there is.
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        global.global.update(cx, |global, cx| {
            global.show(GlobalView::Dashboard, false, window, cx)
        });
        workspace.update(cx, |workspace, cx| {
            workspace.open_scoped_orchestrator(None, window, cx)
        });
    });
    cx.run_until_parked();
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert_eq!(global_window::shown_view(cx), Some(GlobalView::Chat));
        assert!(
            !global_window::chat_starting(cx),
            "the chat there is needs no Start"
        );
        global.global.update(cx, |global, cx| {
            global.show(GlobalView::Dashboard, false, window, cx)
        });
        workspace.update(cx, |workspace, cx| {
            workspace.pending_automation_chat = Some(chat.id.clone());
            cx.notify();
        });
    });
    crate::form_input::test_turn(cx, fixture.handle, |_, cx| {
        assert_eq!(global_window::shown_view(cx), Some(GlobalView::Chat));
        let workspace = workspace.read(cx);
        assert!(workspace.pending_automation_chat.is_none());
        assert!(chat_tabs_of(workspace).is_empty());
    });
    // And the global orchestrator's terminal.
    let terminal_id = uuid::Uuid::new_v4().to_string();
    let terminal = shell(&terminal_id, ShellKind::Orchestrator, None);
    sample(cx, &global, vec![terminal.clone()], vec![chat.clone()]);
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.shells = vec![terminal.clone()];
            workspace.show_shell(&terminal_id, window, cx);
            workspace.show_orchestrator_shell(terminal.clone(), window, cx);
        });
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, _| {
        assert!(
            workspace
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .all(|tab| tab.shell_id() != Some(terminal_id.as_str())),
            "no terminal tab for it here"
        );
    });
    // The window's project and the store's selection are as they were.
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.project_id, fixture.project_id)
    });
    assert_eq!(
        Store::open(&fixture.home)
            .unwrap()
            .snapshot()
            .unwrap()
            .active_project_id,
        None
    );
    release_shell(&terminal_id);
}

#[gpui::test]
fn dashboard_rows_open_their_project_window_and_the_global_window_stays_global(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = window(cx);
    let workspace = fixture.workspace.clone();
    let global = global_beside(&fixture, cx);
    let task: store::Task = serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "project_id": fixture.project_id,
        "title": "Ship it",
        "status": "in_progress",
        "created_at": 1,
        "updated_at": 2,
    }))
    .unwrap();
    let state = workspace.read_with(cx, |workspace, _| {
        let mut state = workspace.state.clone();
        state.tasks.push(task.clone());
        state
    });
    workspace.update(cx, |workspace, _| workspace.state = state.clone());
    global
        .global
        .update(cx, |global, _| global.state = state.clone());
    sample(cx, &global, Vec::new(), Vec::new());
    crate::form_input::test_turn(cx, global.handle, |window, cx| {
        global.global.update(cx, |global, cx| {
            global.dashboard_action(
                DashboardAction::Open(Target::Task {
                    id: task.id.clone(),
                }),
                window,
                cx,
            )
        });
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(
            workspace.selected_task_id.as_deref(),
            Some(task.id.as_str())
        );
        let (pane, tab) = workspace
            .panes
            .iter()
            .find_map(|(id, pane)| {
                pane.tabs
                    .iter()
                    .position(|tab| tab.panel() == Some(PanelKind::Tasks))
                    .map(|index| (*id, index))
            })
            .expect("a Tasks tab");
        assert_eq!(workspace.panes[&pane].active, tab);
        assert_eq!(workspace.project_id, fixture.project_id);
        assert_eq!(global_window::shown_view(cx), Some(GlobalView::Dashboard));
    });
    crate::form_input::test_turn(cx, global.handle, |window, cx| {
        global.global.update(cx, |global, cx| {
            global.dashboard_action(
                DashboardAction::ProjectPanel {
                    project_id: fixture.project_id.clone(),
                    panel: PanelKind::Shells,
                },
                window,
                cx,
            )
        });
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, _| {
        assert!(
            workspace
                .panes
                .values()
                .flat_map(|pane| &pane.tabs)
                .any(|tab| tab.panel() == Some(PanelKind::Shells))
        );
    });
    // A row the window no longer knows is said to be gone, and nothing opens.
    crate::form_input::test_turn(cx, global.handle, |window, cx| {
        global.global.update(cx, |global, cx| {
            global.dashboard_action(
                DashboardAction::Open(Target::Chat {
                    id: uuid::Uuid::new_v4().to_string(),
                    project_id: None,
                    orchestrator: Some(OrchestratorScope::Global),
                }),
                window,
                cx,
            );
            assert_eq!(global.notice.as_deref(), Some("That chat is gone."));
        });
    });
    // The global window is no project's: its rows did not make it one.
    global.global.read_with(cx, |global, _| {
        assert_eq!(global.view, GlobalView::Dashboard)
    });
    assert_eq!(
        Store::open(&fixture.home)
            .unwrap()
            .snapshot()
            .unwrap()
            .active_project_id,
        None
    );
    nothing_was_started(&fixture, cx);
}

#[test]
fn an_older_layout_s_global_tabs_are_the_global_window_s() {
    let terminal = shell("shell-g", ShellKind::Orchestrator, None);
    let ended = ShellSession {
        id: "shell-ended".into(),
        ..terminal.clone()
    };
    let project = shell("shell-p", ShellKind::Project, Some("p1"));
    let live = BTreeMap::from([
        (terminal.id.clone(), terminal.clone()),
        (project.id.clone(), project.clone()),
    ]);
    let global = |id: &str| id == "chat-g";
    let leaves = |tab: SavedTab| belongs_to_global_window(&tab, &live, &global);
    assert!(leaves(SavedTab::Panel {
        panel: PanelKind::Dashboard
    }));
    assert!(leaves(SavedTab::Chat {
        chat_id: "chat-g".into()
    }));
    assert!(leaves(SavedTab::Shell {
        shell_id: "shell-g".into()
    }));
    // Everything else stays where it was.
    assert!(!leaves(SavedTab::Panel {
        panel: PanelKind::Usage
    }));
    assert!(!leaves(SavedTab::Chat {
        chat_id: "chat-p".into()
    }));
    assert!(!leaves(SavedTab::Shell {
        shell_id: "shell-p".into()
    }));
    // A terminal that is not live is not restored anyway.
    assert!(!leaves(SavedTab::Shell { shell_id: ended.id }));
    assert!(!leaves(SavedTab::RemoteShell {
        desktop_id: "h1".into(),
        shell_id: "shell-g".into()
    }));
}

#[gpui::test]
fn an_older_layout_loads_without_its_global_tabs_and_saves_without_them(
    cx: &mut gpui::TestAppContext,
) {
    let fixture = window(cx);
    let workspace = fixture.workspace.clone();
    let chat = global_chat(&fixture.home);
    let legacy: ProjectLayout = serde_json::from_value(serde_json::json!({
        "layout": {"pane": 1},
        "active_pane": 1,
        "panels_initialized": true,
        "sidebar_visible": false,
        "panes": {"1": {
            "tabs": [
                {"kind": "panel", "panel": "usage"},
                {"kind": "panel", "panel": "dashboard"},
                {"kind": "chat", "chat_id": chat.id},
            ],
            "active_tab_key": "panel:dashboard",
        }},
    }))
    .unwrap();
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace
                .layouts
                .save_chat_state(&fixture.project_id, &legacy, &[], None)
                .unwrap();
            workspace.shared_chats = Some(vec![chat.clone()]);
            workspace.layout_ready = false;
            workspace.load_project(window, cx);
        });
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, cx| {
        let tabs = workspace
            .panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .map(|tab| tab.saved())
            .collect::<Vec<_>>();
        assert_eq!(
            tabs,
            [Some(SavedTab::Panel {
                panel: PanelKind::Usage
            })]
        );
        assert_eq!(
            workspace.notice.as_deref(),
            Some(global_window::MOVED_NOTICE)
        );
        // Nothing opens the global window by itself; the menu and the shortcuts do.
        assert!(global_window::current(cx).is_none());
    });
    let saved = workspace.read_with(cx, |workspace, _| {
        workspace
            .layouts
            .load(&fixture.project_id)
            .unwrap()
            .unwrap()
    });
    let kept = saved
        .panes
        .values()
        .flat_map(|pane| &pane.tabs)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        kept,
        [SavedTab::Panel {
            panel: PanelKind::Usage
        }]
    );
    let _ = chat_info;
}

/// The window works out each chat tab's image scope from the chat's identity and the store,
/// and keeps it in step. (The global chat's is the global window's; see its tests.)
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
    let ordinary_tab = {
        let ordinary = ordinary.clone();
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
                workspace.shared_chats = Some(vec![ordinary.clone()]);
                // A project chat's tab is recorded in the project's shared tab store.
                project_tabs::TabStore::at(&home, &project)
                    .unwrap()
                    .reconcile(&[project_tabs::Session::chat(&ordinary)])
                    .unwrap();
                workspace.shared_tabs = Some(Vec::new());
                workspace.show_existing_chat(ordinary.clone(), window, cx);
                assert_eq!(workspace.notice, None);
                let ordinary_tab = tab_of(workspace, &ordinary.id);
                ordinary_tab.update(cx, |view, _| view.adopt_info_for_tests(ordinary.clone()));
                workspace.refresh_chat_image_scopes(cx);
                tabs = Some(ordinary_tab);
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
        home.join("chats").join(&ordinary.id).join("attachments"),
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
    // The store forgets the feature worktree; the next refresh takes it out of the scope.
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
}
