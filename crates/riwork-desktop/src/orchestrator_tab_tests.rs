//! A project orchestrator's tab is its project's: a window that switches to another project
//! leaves it out (a carried locked pane, a layout saved with it, or a tab restored before the
//! chat inventory said what it was), and finds it again on the way back. Only the window's
//! view and layout change: the chat, its draft and settings, its project's shared tabs and
//! its claim for that project's window stay as they are, and nothing is started.

use super::*;
use crate::chat_tabs::Coordinator;

/// A private data directory with two local projects, removed with the guard.
struct Home {
    path: PathBuf,
    a: Project,
    b: Project,
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn home() -> Home {
    let path = std::env::temp_dir().join(format!("rw-orch-tabs-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    // As the window spells it (macOS `/var` is a link).
    let path = path.canonicalize().unwrap();
    let project = |name: &str| -> Project {
        let root = path.join("work").join(name);
        std::fs::create_dir_all(&root).unwrap();
        serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "name": name,
            "root": root,
            "created_at": 1
        }))
        .unwrap()
    };
    let (a, b) = (project("alpha"), project("beta"));
    Home { path, a, b }
}

/// A chat of `project`, its orchestrator when `orchestrator`.
fn chat(project: &Project, orchestrator: bool) -> ChatInfo {
    let mut value = serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "provider": "codex",
        "project_id": project.id,
        "cwd": project.root,
        "title": "Chat",
        "created_at_unix": 1,
        "model": "gpt-5.5",
        "state": {"state": "idle"},
    });
    if orchestrator {
        value["orchestrator"] = serde_json::json!({"scope": "project", "project_id": project.id});
        value["title"] = "Project orchestrator".into();
    }
    serde_json::from_value(value).unwrap()
}

/// Save the chat as the host keeps it, with a draft in its message box.
fn keep(home: &Path, info: &ChatInfo) {
    let dir = chat::log::chat_dir(home, &info.id).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("info.json"), serde_json::to_vec(info).unwrap()).unwrap();
    std::fs::write(
        dir.join("events.jsonl"),
        b"{\"kind\":\"user\",\"text\":\"hello\"}\n",
    )
    .unwrap();
    let drafts = home.join(chat_drafts::DIR_NAME);
    std::fs::create_dir_all(&drafts).unwrap();
    std::fs::write(drafts.join(&info.id), "half a thought").unwrap();
}

/// What the host and the draft store hold for the chat, byte for byte.
fn kept(home: &Path, id: &str) -> Vec<Vec<u8>> {
    let dir = chat::log::chat_dir(home, id).unwrap();
    vec![
        std::fs::read(dir.join("info.json")).unwrap(),
        std::fs::read(dir.join("events.jsonl")).unwrap(),
        std::fs::read(home.join(chat_drafts::DIR_NAME).join(id)).unwrap(),
    ]
}

fn chat_tab(chat_id: &str) -> SavedTab {
    SavedTab::Chat {
        chat_id: chat_id.to_owned(),
    }
}

fn panel_tab(panel: PanelKind) -> SavedTab {
    SavedTab::Panel { panel }
}

fn pane(tabs: Vec<SavedTab>) -> SavedPane {
    // A panel is the selected tab: no chat's view is drawn, so none starts following its
    // chat through a host.
    let active_tab_key = tabs.first().map(SavedTab::key);
    SavedPane {
        tabs,
        active_tab_key,
        ..SavedPane::default()
    }
}

/// A locked navigation pane (1) beside a work pane (2).
fn layout(locked: Vec<SavedTab>, work: Vec<SavedTab>) -> ProjectLayout {
    ProjectLayout {
        layout: Layout::navigation_beside(1, Layout::Pane(2)),
        panes: BTreeMap::from([(1, pane(locked)), (2, pane(work))]),
        active_pane: 2,
        panels_initialized: true,
        detached_shell_ids: HashSet::new(),
        detached_chat_ids: HashSet::new(),
        chat_dismissal_revision: 0,
        selected_worktree_id: None,
        selected_task_id: None,
        sidebar_visible: true,
        window_size: None,
        locked_panes: Some(HashSet::from([1])),
        main_pane: Some(2),
    }
}

/// A window on `start`, loaded from its saved layout, as it opens.
fn window(
    cx: &mut gpui::TestAppContext,
    home: &Home,
    start: &Project,
) -> (gpui::AnyWindowHandle, Entity<Workspace>) {
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
    let startup = WorkspaceStartup {
        store: Store::open(&home.path).unwrap(),
        layouts: LayoutStore::open(&home.path).unwrap(),
        settings_store: SettingsStore::open(&home.path).unwrap(),
        sessions: SessionManager::at(home.path.clone()).unwrap(),
        state: State {
            projects: vec![home.a.clone(), home.b.clone()],
            ..Default::default()
        },
        project: start.clone(),
        remote: None,
    };
    let (handle, workspace) = crate::form_input::test_window(cx, move |window, cx| {
        Workspace::build(startup, None, window, cx, false)
    });
    crate::form_input::test_turn(cx, handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.layout_ready = false;
            workspace.load_project(window, cx);
        });
    });
    (handle, workspace)
}

fn switch(
    cx: &mut gpui::TestAppContext,
    handle: gpui::AnyWindowHandle,
    workspace: &Entity<Workspace>,
    to: &Project,
) {
    let to = to.id.clone();
    crate::form_input::test_turn(cx, handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.activate_project(&to, false, window, cx)
        });
    });
}

/// Each chat tab of the window: its chat, its pane, its place, and whether the pane is locked.
fn chat_tabs(workspace: &Workspace) -> Vec<(String, PaneId, usize, bool)> {
    let locked = workspace.locked_panes.clone().unwrap_or_default();
    workspace
        .panes
        .iter()
        .flat_map(|(id, pane)| {
            let locked = locked.contains(id);
            pane.tabs
                .iter()
                .enumerate()
                .filter_map(move |(index, tab)| {
                    tab.chat_id()
                        .map(|chat| (chat.to_owned(), *id, index, locked))
                })
        })
        .collect()
}

fn has_chat(workspace: &Workspace, id: &str) -> bool {
    chat_tabs(workspace).iter().any(|(chat, ..)| chat == id)
}

fn saved_chats(layouts: &LayoutStore, project: &str) -> Vec<String> {
    layouts
        .load(project)
        .unwrap()
        .unwrap()
        .panes
        .values()
        .flat_map(|pane| &pane.tabs)
        .filter_map(|tab| match tab {
            SavedTab::Chat { chat_id } => Some(chat_id.clone()),
            _ => None,
        })
        .collect()
}

/// The project's shared tab for the chat: still listed, and not hidden.
fn still_a_member(home: &Path, project: &str, chat: &str) -> bool {
    project_tabs::TabStore::at(home, project)
        .unwrap()
        .snapshot()
        .unwrap()
        .entries
        .iter()
        .any(|entry| entry.key == format!("chat:{chat}") && !entry.hidden)
}

/// Nothing followed a chat through a host, and nothing was started.
fn nothing_started(workspace: &Workspace, cx: &App) {
    assert!(workspace.orchestrators_starting.is_empty());
    for pane in workspace.panes.values() {
        for tab in &pane.tabs {
            if let Some(view) = tab.chat() {
                assert!(!view.read(cx).following_for_tests());
            }
        }
    }
}

#[gpui::test]
fn a_project_orchestrator_tab_stays_with_its_project_when_the_window_switches(
    cx: &mut gpui::TestAppContext,
) {
    let home = home();
    let (a_orch, a_pinned, b_orch) = (
        chat(&home.a, true),
        chat(&home.a, false),
        chat(&home.b, true),
    );
    for info in [&a_orch, &a_pinned, &b_orch] {
        keep(&home.path, info);
    }
    // Alpha's orchestrator and an ordinary chat of alpha's are kept in alpha's locked pane.
    // Beta's saved layout already holds alpha's orchestrator (saved by a build that carried
    // it) beside beta's own.
    let layouts = LayoutStore::open(&home.path).unwrap();
    layouts
        .save_chat_state(
            &home.a.id,
            &layout(
                vec![
                    panel_tab(PanelKind::Projects),
                    chat_tab(&a_orch.id),
                    chat_tab(&a_pinned.id),
                ],
                vec![panel_tab(PanelKind::Usage)],
            ),
            &[],
            None,
        )
        .unwrap();
    layouts
        .save_chat_state(
            &home.b.id,
            &layout(
                vec![panel_tab(PanelKind::Projects)],
                vec![
                    panel_tab(PanelKind::Usage),
                    chat_tab(&b_orch.id),
                    chat_tab(&a_orch.id),
                ],
            ),
            &[],
            None,
        )
        .unwrap();
    // Alpha's orchestrator is a member of alpha's shared tabs.
    project_tabs::TabStore::at(&home.path, &home.a.id)
        .unwrap()
        .reconcile(&[project_tabs::Session::chat(&a_orch)])
        .unwrap();
    let before = kept(&home.path, &a_orch.id);
    let elsewhere = Coordinator::new();

    let (handle, workspace) = window(cx, &home, &home.a);
    let at_alpha = workspace.read_with(cx, |workspace, _| chat_tabs(workspace));
    assert!(
        at_alpha.contains(&(a_orch.id.clone(), 1, 1, true)),
        "{at_alpha:?}"
    );
    assert!(!elsewhere.available(&a_orch.id), "alpha's window holds it");

    // To beta: alpha's orchestrator is gone from every pane, the locked one included, while
    // the ordinary chat locked with it comes along as locked tabs always do. Beta's own
    // orchestrator is there.
    switch(cx, handle, &workspace, &home.b);
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.project_id, home.b.id);
        assert!(
            !has_chat(workspace, &a_orch.id),
            "{:?}",
            chat_tabs(workspace)
        );
        assert!(has_chat(workspace, &b_orch.id));
        assert!(
            chat_tabs(workspace)
                .iter()
                .any(|(chat, _, _, locked)| chat == &a_pinned.id && *locked)
        );
        nothing_started(workspace, cx);
    });
    // No claim on it is left, so a window showing alpha can take it; beta's own is held.
    assert!(elsewhere.available(&a_orch.id));
    assert!(!elsewhere.available(&b_orch.id));
    // Beta's layout no longer names it; alpha's still does, and alpha's shared tabs list it.
    assert!(!saved_chats(&layouts, &home.b.id).contains(&a_orch.id));
    assert!(saved_chats(&layouts, &home.b.id).contains(&b_orch.id));
    assert!(saved_chats(&layouts, &home.a.id).contains(&a_orch.id));
    assert!(still_a_member(&home.path, &home.a.id, &a_orch.id));

    // Back to alpha: its orchestrator is there again, in the locked pane where it was, for
    // the same chat; beta's is not carried along.
    switch(cx, handle, &workspace, &home.a);
    workspace.read_with(cx, |workspace, cx| {
        let tabs = chat_tabs(workspace);
        assert!(tabs.contains(&(a_orch.id.clone(), 1, 1, true)), "{tabs:?}");
        assert!(!has_chat(workspace, &b_orch.id), "{tabs:?}");
        let view = workspace
            .panes
            .values()
            .flat_map(|pane| &pane.tabs)
            .find(|tab| tab.chat_id() == Some(a_orch.id.as_str()))
            .and_then(Tab::chat)
            .unwrap();
        assert_eq!(
            view.read(cx).identity_for_tests(cx).0,
            Some(a_orch.id.clone())
        );
        nothing_started(workspace, cx);
    });
    assert!(!elsewhere.available(&a_orch.id));
    assert!(elsewhere.available(&b_orch.id));
    // The chat, its history, its settings and its draft are as they were.
    assert_eq!(kept(&home.path, &a_orch.id), before);
    assert!(still_a_member(&home.path, &home.a.id, &a_orch.id));
    // Nothing made another chat.
    assert_eq!(
        std::fs::read_dir(chat::log::chats_dir(&home.path))
            .unwrap()
            .count(),
        3
    );
}

#[gpui::test]
fn an_orchestrator_in_a_work_pane_is_left_behind_and_found_again(cx: &mut gpui::TestAppContext) {
    let home = home();
    let (a_orch, b_orch) = (chat(&home.a, true), chat(&home.b, true));
    keep(&home.path, &a_orch);
    keep(&home.path, &b_orch);
    let layouts = LayoutStore::open(&home.path).unwrap();
    layouts
        .save_chat_state(
            &home.a.id,
            &layout(
                vec![panel_tab(PanelKind::Projects)],
                vec![panel_tab(PanelKind::Usage), chat_tab(&a_orch.id)],
            ),
            &[],
            None,
        )
        .unwrap();
    layouts
        .save_chat_state(
            &home.b.id,
            &layout(
                vec![panel_tab(PanelKind::Projects)],
                vec![panel_tab(PanelKind::Usage), chat_tab(&b_orch.id)],
            ),
            &[],
            None,
        )
        .unwrap();
    let (handle, workspace) = window(cx, &home, &home.a);
    assert!(workspace.read_with(cx, |workspace, _| has_chat(workspace, &a_orch.id)));
    switch(cx, handle, &workspace, &home.b);
    workspace.read_with(cx, |workspace, _| {
        assert!(!has_chat(workspace, &a_orch.id));
        assert!(has_chat(workspace, &b_orch.id));
    });
    switch(cx, handle, &workspace, &home.a);
    workspace.read_with(cx, |workspace, cx| {
        assert!(has_chat(workspace, &a_orch.id));
        assert!(!has_chat(workspace, &b_orch.id));
        nothing_started(workspace, cx);
    });
}

#[gpui::test]
fn a_tab_restored_before_its_chat_was_known_leaves_once_the_inventory_names_it(
    cx: &mut gpui::TestAppContext,
) {
    let home = home();
    // Nothing on disk says what these chats are, and the inventory has not been read: the
    // window restores them as it restores any chat.
    let (a_orch, a_ordinary, b_orch) = (
        chat(&home.a, true),
        chat(&home.a, false),
        chat(&home.b, true),
    );
    let layouts = LayoutStore::open(&home.path).unwrap();
    layouts
        .save_chat_state(
            &home.b.id,
            &layout(
                vec![panel_tab(PanelKind::Projects)],
                vec![
                    panel_tab(PanelKind::Usage),
                    chat_tab(&a_orch.id),
                    chat_tab(&a_ordinary.id),
                    chat_tab(&b_orch.id),
                ],
            ),
            &[],
            None,
        )
        .unwrap();
    project_tabs::TabStore::at(&home.path, &home.a.id)
        .unwrap()
        .reconcile(&[project_tabs::Session::chat(&a_orch)])
        .unwrap();
    let elsewhere = Coordinator::new();
    let (handle, workspace) = window(cx, &home, &home.b);
    workspace.read_with(cx, |workspace, _| {
        assert!(workspace.shared_chats.is_none());
        for id in [&a_orch.id, &a_ordinary.id, &b_orch.id] {
            assert!(has_chat(workspace, id));
        }
    });
    assert!(!elsewhere.available(&a_orch.id));

    // The inventory arrives: alpha's orchestrator leaves beta's window and its layout; the
    // ordinary chat and beta's own orchestrator stay.
    let inventory = vec![a_orch.clone(), a_ordinary.clone(), b_orch.clone()];
    crate::form_input::test_turn(cx, handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            assert!(workspace.take_chat_inventory(inventory, HashSet::new(), window, cx));
        });
    });
    workspace.read_with(cx, |workspace, cx| {
        assert!(!has_chat(workspace, &a_orch.id));
        assert!(has_chat(workspace, &a_ordinary.id));
        assert!(has_chat(workspace, &b_orch.id));
        nothing_started(workspace, cx);
    });
    assert!(elsewhere.available(&a_orch.id));
    assert!(!saved_chats(&layouts, &home.b.id).contains(&a_orch.id));
    assert!(still_a_member(&home.path, &home.a.id, &a_orch.id));

    // Reading it again, or loading the same project again, does not bring it back.
    let inventory = vec![a_orch.clone(), a_ordinary.clone(), b_orch.clone()];
    crate::form_input::test_turn(cx, handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.take_chat_inventory(inventory, HashSet::new(), window, cx);
            workspace.layout_ready = false;
            workspace.load_project(window, cx);
        });
    });
    workspace.read_with(cx, |workspace, _| {
        assert!(!has_chat(workspace, &a_orch.id));
        assert!(has_chat(workspace, &a_ordinary.id));
        assert!(has_chat(workspace, &b_orch.id));
    });
    assert!(elsewhere.available(&a_orch.id));
}

#[gpui::test]
fn only_another_projects_orchestrator_counts_as_foreign(cx: &mut gpui::TestAppContext) {
    let home = home();
    let layouts = LayoutStore::open(&home.path).unwrap();
    layouts
        .save_chat_state(
            &home.b.id,
            &layout(
                vec![panel_tab(PanelKind::Projects)],
                vec![panel_tab(PanelKind::Usage)],
            ),
            &[],
            None,
        )
        .unwrap();
    let (_, workspace) = window(cx, &home, &home.b);
    let shell = |id: &str, kind: &str, project: Option<&str>| {
        let mut shell: ShellSession = serde_json::from_value(serde_json::json!({
            "id": id,
            "project_id": project,
            "worktree_id": null,
            "kind": kind,
            "cwd": "/tmp",
            "command": null,
            "created_at_unix": 0
        }))
        .unwrap();
        shell.alive = true;
        shell
    };
    let live = [
        shell("a-orch", "orchestrator", Some(&home.a.id)),
        shell("b-orch", "orchestrator", Some(&home.b.id)),
        shell("global", "orchestrator", None),
        shell("a-shell", "project", Some(&home.a.id)),
    ]
    .into_iter()
    .map(|shell| (shell.id.clone(), shell))
    .collect::<BTreeMap<_, _>>();
    let (a_orch, a_chat, b_orch) = (
        chat(&home.a, true),
        chat(&home.a, false),
        chat(&home.b, true),
    );
    let mut global = chat(&home.a, false);
    global.project_id = None;
    global.orchestrator = Some(OrchestratorScope::Global);
    workspace.update(cx, |workspace, _| {
        workspace.shared_chats = Some(vec![
            a_orch.clone(),
            a_chat.clone(),
            b_orch.clone(),
            global.clone(),
        ]);
        let foreign = |workspace: &Workspace, tab: SavedTab| {
            workspace.is_foreign_orchestrator_tab(&tab, &live)
        };
        let shell_tab = |id: &str| SavedTab::Shell {
            shell_id: id.to_owned(),
        };
        // Alpha's orchestrator, as a chat or a terminal, is foreign to beta's window.
        assert!(foreign(workspace, chat_tab(&a_orch.id)));
        assert!(foreign(workspace, shell_tab("a-orch")));
        // Beta's own, ordinary chats and shells of alpha's (carried as they always are),
        // the global orchestrator (the global window's own rule) and panels are not.
        for tab in [
            chat_tab(&b_orch.id),
            chat_tab(&a_chat.id),
            chat_tab(&global.id),
            shell_tab("b-orch"),
            shell_tab("a-shell"),
            shell_tab("global"),
            panel_tab(PanelKind::Usage),
        ] {
            assert!(!foreign(workspace, tab.clone()), "{tab:?}");
        }
        // A chat nothing says anything about is not called foreign.
        assert!(!foreign(
            workspace,
            chat_tab(&uuid::Uuid::new_v4().to_string())
        ));
        // A window showing a project on another Mac shows none of this Mac's project
        // orchestrators either.
        workspace.project_id =
            remote_tree::project_key(&uuid::Uuid::new_v4().to_string(), &home.b.id);
        assert!(workspace.is_remote());
        assert!(foreign(workspace, chat_tab(&b_orch.id)));
        assert!(foreign(workspace, shell_tab("b-orch")));
        workspace.project_id = home.b.id.clone();
    });
}
