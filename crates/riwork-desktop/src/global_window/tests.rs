use super::*;
use gpui_kit::test::TestWindowExt;

/// An explicit Start reaches no chat host in a test: a test build must never start one.
fn offline_host(_: &Path) -> Result<PathBuf, String> {
    Err("no chat host in this test".to_owned())
}

/// A private data directory, removed with the guard.
pub(crate) struct Home(pub(crate) PathBuf);

impl Home {
    pub(crate) fn new() -> Self {
        let path = std::env::temp_dir().join(format!("rw-global-window-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        // As the window and the chat host spell it (macOS `/var` is a link).
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) struct GlobalFixture {
    pub(crate) handle: gpui::AnyWindowHandle,
    pub(crate) global: Entity<GlobalWindow>,
}

pub(crate) fn init_app(cx: &mut gpui::TestAppContext) {
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
}

/// The global window over the data directory `home`, without timers, sampling or terminals:
/// what it knows comes from the test (`take_sample`).
pub(crate) fn open_global(
    cx: &mut gpui::TestAppContext,
    home: &Path,
    saved: GlobalWindowState,
) -> GlobalFixture {
    let store = Store::open(home).unwrap();
    let sessions = SessionManager::at(home.to_path_buf()).unwrap();
    let startup = GlobalStartup {
        state: store.snapshot().unwrap(),
        store,
        settings_store: SettingsStore::open(home).unwrap(),
        saved: GlobalWindowStore::at(sessions.state_home()),
        host: HostConfig::offline(),
        ensure_host: offline_host,
        sessions,
    };
    let (handle, global) = crate::form_input::test_window(cx, move |window, cx| {
        GlobalWindow::new(startup, saved, window, cx, false)
    });
    GlobalFixture { handle, global }
}

/// What the window would have read: these sessions and these chats.
pub(crate) fn sample(
    cx: &mut gpui::TestAppContext,
    fixture: &GlobalFixture,
    shells: Vec<ShellSession>,
    chats: Vec<ChatInfo>,
) {
    let global = fixture.global.clone();
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        global.update(cx, |global, cx| {
            global.take_sample(shells, Some(BTreeMap::new()), Some(chats), window, cx)
        });
    });
}

pub(crate) fn chat_info(value: serde_json::Value) -> ChatInfo {
    let mut base = serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "provider": "claude",
        "cwd": "/tmp",
        "title": "Chat",
        "created_at_unix": 1,
        "state": {"state": "idle"},
    });
    for (key, field) in value.as_object().unwrap() {
        base[key] = field.clone();
    }
    serde_json::from_value(base).unwrap()
}

pub(crate) fn global_chat(home: &Path) -> ChatInfo {
    chat_info(serde_json::json!({
        "cwd": home.join("orchestrator"),
        "title": "Global orchestrator",
        "state": {"state": "waiting"},
        "orchestrator": {"scope": "global"},
    }))
}

pub(crate) fn shell(id: &str, kind: ShellKind, project_id: Option<&str>) -> ShellSession {
    ShellSession {
        user_opened: false,
        parent_id: None,
        id: id.to_owned(),
        project_id: project_id.map(str::to_owned),
        worktree_id: None,
        kind,
        cwd: PathBuf::from("/tmp"),
        command: None,
        editor_path: None,
        harness: Some(HarnessKind::Codex),
        codex_account_id: None,
        codex_account_label: None,
        codex_account_email: None,
        codex_home: None,
        unrestricted: false,
        orchestrator_skill_loaded: false,
        orchestrator_skill_version: None,
        orchestrator_project_root: None,
        created_at_unix: 0,
        alive: true,
    }
}

fn shown_chat(global: &GlobalWindow) -> Option<(String, Entity<ChatView>)> {
    match &global.shown {
        Shown::Chat { id, view } => Some((id.clone(), view.clone())),
        _ => None,
    }
}

fn shown_terminal(global: &GlobalWindow) -> Option<String> {
    match &global.shown {
        Shown::Terminal { shell_id, .. } => Some(shell_id.clone()),
        _ => None,
    }
}

/// Nothing made a chat, an orchestrator or a session, and nothing was written for a project.
fn nothing_was_started_or_saved_for_a_project(home: &Path) {
    for made in [
        "chats",
        "orchestrator",
        "sessions.json",
        "layouts.json",
        "project-tabs",
    ] {
        assert!(!home.join(made).exists(), "{made} was made");
    }
}

#[gpui::test]
fn the_dashboard_opens_in_its_own_window_and_starts_or_selects_nothing(
    cx: &mut gpui::TestAppContext,
) {
    init_app(cx);
    let home = Home::new();
    let root = home.0.join("work/alpha");
    std::fs::create_dir_all(&root).unwrap();
    let store = Store::open(&home.0).unwrap();
    let project = store.add_project(root, None).unwrap();
    let before = store.snapshot().unwrap();
    let fixture = open_global(cx, &home.0, GlobalWindowState::default());
    let global = fixture.global.clone();
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        // The dashboard is this window's whole body.
        assert!(window.try_find("dashboard-refresh").is_some());
        assert!(window.try_find("global-chat-start").is_none());
        assert_eq!(current(cx).map(|(handle, _)| handle), Some(fixture.handle));
        assert_eq!(shown_view(cx), Some(GlobalView::Dashboard));
        // It has the keys from the start (⌘W closes it before anything was clicked).
        assert!(global.read(cx).focus.is_focused(window));
    });
    sample(cx, &fixture, Vec::new(), Vec::new());
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        // The project is on the dashboard; the window is no project's.
        let overview = global.read(cx).overview();
        assert_eq!(overview.projects.len(), 1);
        assert_eq!(overview.projects[0].id, project.id);
        window.click("global-view-chat", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        // There is no orchestrator: the Chat view says so and offers the one control that
        // may start one, but showing it started nothing.
        assert_eq!(global.read(cx).view, GlobalView::Chat);
        assert!(!global.read(cx).starting);
        let start = window.find("global-chat-start");
        assert_eq!(start.label(), Some("Start global orchestrator"));
        window.click("global-view-dashboard", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |_, cx| {
        assert_eq!(global.read(cx).view, GlobalView::Dashboard);
    });
    nothing_was_started_or_saved_for_a_project(&home.0);
    // Its state is its own file; the store's selection is as it was.
    let saved = GlobalWindowStore::at(&home.0).load();
    assert_eq!(saved.view, GlobalView::Dashboard);
    assert_eq!(Store::open(&home.0).unwrap().snapshot().unwrap(), before);
}

#[gpui::test]
fn the_chat_view_is_the_global_chat_with_its_identity_draft_and_image_scope(
    cx: &mut gpui::TestAppContext,
) {
    init_app(cx);
    let home = Home::new();
    let root = home.0.join("work/alpha");
    std::fs::create_dir_all(&root).unwrap();
    let project = Store::open(&home.0)
        .unwrap()
        .add_project(root.clone(), None)
        .unwrap();
    let chat = global_chat(&home.0);
    let project_orchestrator = chat_info(serde_json::json!({
        "project_id": project.id,
        "cwd": root,
        "orchestrator": {"scope": "project", "project_id": project.id},
    }));
    cx.update(|cx| {
        crate::chat_drafts::init(&home.0, cx);
        crate::chat_drafts::remember(&chat.id, "half a thought", cx);
    });
    let fixture = open_global(cx, &home.0, GlobalWindowState::default());
    let global = fixture.global.clone();
    sample(
        cx,
        &fixture,
        Vec::new(),
        vec![project_orchestrator.clone(), chat.clone()],
    );
    // The Dashboard does not look for the orchestrator at all.
    global.read_with(cx, |global, _| assert!(shown_chat(global).is_none()));
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        global.update(cx, |global, cx| {
            global.show(GlobalView::Chat, false, window, cx)
        });
    });
    let view = global.read_with(cx, |global, cx| {
        let (id, view) = shown_chat(global).expect("the global chat is shown");
        assert_eq!(id, chat.id, "the global orchestrator's, not the project's");
        // The chat's own view: its id and the draft kept for it.
        assert_eq!(
            view.read(cx).identity_for_tests(cx),
            (Some(chat.id.clone()), "half a thought".to_owned())
        );
        view
    });
    // No project window can adopt it as a tab while this window shows it.
    let other = chat_tabs::Coordinator::new();
    assert!(!other.claim(&chat.id, false));

    // Its image cards read from every project and the global orchestrator's folders.
    crate::form_input::test_turn(cx, fixture.handle, |_, cx| {
        view.update(cx, |view, _| view.adopt_info_for_tests(chat.clone()));
        global.update(cx, |global, cx| global.refresh_image_scope(cx));
    });
    let mut expected = vec![
        root.clone(),
        home.0.join("orchestrator"),
        home.0.join("orchestrators/projects").join(&project.id),
        home.0.join("chats").join(&chat.id).join("attachments"),
    ];
    expected.sort();
    view.read_with(cx, |view, _| {
        assert_eq!(view.image_scope().roots(), expected.as_slice())
    });
    // It follows the store, and a listing that no longer calls the chat global takes every
    // folder away.
    crate::form_input::test_turn(cx, fixture.handle, |_, cx| {
        global.update(cx, |global, cx| {
            global.state.projects.clear();
            global.refresh_image_scope(cx);
        });
    });
    view.read_with(cx, |view, _| {
        assert!(!view.image_scope().roots().contains(&root));
        assert!(
            view.image_scope()
                .roots()
                .contains(&home.0.join("orchestrator"))
        );
    });
    let mut demoted = chat.clone();
    demoted.orchestrator = None;
    crate::form_input::test_turn(cx, fixture.handle, |_, cx| {
        global.update(cx, |global, cx| {
            global.chats = Some(vec![demoted]);
            global.refresh_image_scope(cx);
        });
    });
    view.read_with(cx, |view, _| assert!(view.image_scope().roots().is_empty()));
    global.update(cx, |global, _| {
        global.chats = Some(vec![project_orchestrator.clone(), chat.clone()])
    });

    // The Dashboard and back, and an explicit Chat: the same view, and nothing made.
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        global.update(cx, |global, cx| {
            global.show(GlobalView::Dashboard, false, window, cx);
            global.show(GlobalView::Chat, true, window, cx);
        });
    });
    global.read_with(cx, |global, _| {
        assert!(!global.starting, "the chat there is needs no Start");
        assert_eq!(
            shown_chat(global).map(|(_, shown)| shown),
            Some(view.clone())
        );
    });

    // Closing the window drops its view only.
    cx.update_window(fixture.handle, |_, window, _| window.remove_window())
        .unwrap();
    let weak = global.downgrade();
    drop((fixture, global, view));
    cx.run_until_parked();
    cx.update(|_| assert!(weak.upgrade().is_none(), "the window's view is let go"));
    cx.update(|cx| {
        assert!(current(cx).is_none());
        assert!(
            cx.try_global::<Open>()
                .is_some_and(|open| open.window.is_none())
        );
    });
    assert!(other.claim(&chat.id, false), "its claim went with it");
    other.release(&chat.id);
    assert_eq!(
        GlobalWindowStore::at(&home.0).load().view,
        GlobalView::Chat,
        "it reopens where it was"
    );
    assert!(
        !home.0.join("chats").exists(),
        "the chat is the host's: nothing was deleted or made"
    );

    // Open again: the same chat, and its draft.
    let saved = GlobalWindowStore::at(&home.0).load();
    let again = open_global(cx, &home.0, saved);
    sample(
        cx,
        &again,
        Vec::new(),
        vec![project_orchestrator, chat.clone()],
    );
    again.global.read_with(cx, |global, cx| {
        let (id, view) = shown_chat(global).expect("shown again");
        assert_eq!(id, chat.id);
        assert_eq!(
            view.read(cx).identity_for_tests(cx),
            (Some(chat.id.clone()), "half a thought".to_owned())
        );
    });
}

#[gpui::test]
fn a_terminal_orchestrator_is_shown_as_itself_until_it_ends(cx: &mut gpui::TestAppContext) {
    init_app(cx);
    let home = Home::new();
    let terminal_id = uuid::Uuid::new_v4().to_string();
    let terminal = shell(&terminal_id, ShellKind::Orchestrator, None);
    let fixture = open_global(
        cx,
        &home.0,
        GlobalWindowState {
            view: GlobalView::Chat,
            ..Default::default()
        },
    );
    let global = fixture.global.clone();
    // A live terminal wins over a chat, as `orchestrators::find` has it.
    sample(
        cx,
        &fixture,
        vec![terminal.clone()],
        vec![global_chat(&home.0)],
    );
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert_eq!(shown_terminal(global.read(cx)), Some(terminal_id.clone()));
        assert!(window.try_find("global-chat-start").is_none());
        // This window holds its terminal; no project window adds a tab for it.
        assert!(!claim_shell(&terminal_id));
        // An explicit Chat opens it as it is: nothing converts or replaces it.
        global.update(cx, |global, cx| {
            global.show(GlobalView::Chat, true, window, cx)
        });
        assert!(!global.read(cx).starting);
        assert_eq!(shown_terminal(global.read(cx)), Some(terminal_id.clone()));
    });
    // Its session ends: the window offers to start one, and lets the terminal go.
    let mut ended = terminal.clone();
    ended.alive = false;
    sample(cx, &fixture, vec![ended], Vec::new());
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(matches!(global.read(cx).shown, Shown::Nothing));
        assert!(window.try_find("global-chat-start").is_some());
    });
    assert!(claim_shell(&terminal_id), "released");
    release_shell(&terminal_id);
    nothing_was_started_or_saved_for_a_project(&home.0);
}

#[gpui::test]
fn only_an_explicit_start_asks_for_a_new_orchestrator(cx: &mut gpui::TestAppContext) {
    init_app(cx);
    let home = Home::new();
    SettingsStore::open(&home.0)
        .unwrap()
        .update(|settings| settings.orchestrator_mode = settings::OrchestratorMode::Chat)
        .unwrap();
    let fixture = open_global(cx, &home.0, GlobalWindowState::default());
    let global = fixture.global.clone();
    sample(cx, &fixture, Vec::new(), Vec::new());
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        // Showing Chat looks; it never makes.
        global.update(cx, |global, cx| {
            global.show(GlobalView::Chat, false, window, cx)
        });
        assert!(!global.read(cx).starting);
    });
    assert!(!home.0.join("chats").exists());
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("global-chat-start", cx);
    });
    // The click went to `orchestrators::create`, which asked the chat host to make the chat;
    // there is none in a test, and the window says why instead of showing a chat.
    global.read_with(cx, |global, _| {
        assert!(!global.starting);
        assert!(matches!(global.shown, Shown::Nothing));
        let notice = global.notice.clone().unwrap_or_default();
        assert!(notice.contains("no chat host in this test"), "{notice}");
    });
}

#[gpui::test]
fn every_entry_reuses_the_one_global_window_and_a_reload_records_it_as_itself(
    cx: &mut gpui::TestAppContext,
) {
    init_app(cx);
    let home = Home::new();
    // A state an older build saved: quiet projects shown, no filter of its own.
    let fixture = open_global(
        cx,
        &home.0,
        GlobalWindowState {
            view: GlobalView::Dashboard,
            show_quiet: true,
            ..GlobalWindowState::default()
        },
    );
    let windows = cx.update(|cx| cx.windows().len());
    cx.update(|cx| open(GlobalView::Chat, false, cx));
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(cx.windows().len(), windows, "no second window");
        assert_eq!(shown_view(cx), Some(GlobalView::Chat));
        assert_eq!(open_window(None, cx).unwrap(), fixture.handle);
    });
    cx.update(|cx| open(GlobalView::Dashboard, false, cx));
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(shown_view(cx), Some(GlobalView::Dashboard)));

    // A reload records it with its own kind and state, naming no project and no layout.
    let recorded = cx.update(|cx| runtime_windows(cx, true));
    assert_eq!(recorded.len(), 1);
    let window = &recorded[0];
    assert!(window.is_global() && window.restored());
    assert_eq!(
        (window.project_id.as_ref(), window.layout.as_ref()),
        (None, None)
    );
    assert_eq!(window.path, home.0.join(runtime::GLOBAL_WINDOW_PATH));
    assert!(
        !window.path.exists(),
        "nothing a build could take for a project"
    );
    // The older state's meaning is kept: every project listed, now as the All filter.
    let mut expected = GlobalWindowState {
        view: GlobalView::Dashboard,
        ..GlobalWindowState::default()
    };
    expected.set_filter(DashboardFilter::All);
    assert_eq!(window.global, Some(expected));
    let text = serde_json::to_string(window).unwrap();
    assert!(text.contains("\"kind\":\"global\""), "{text}");
    // Saving for a reload wrote its own file and nothing of a project's.
    let saved = GlobalWindowStore::at(&home.0).load();
    assert!(saved.show_quiet);
    assert_eq!(saved.filter(), DashboardFilter::All);
    nothing_was_started_or_saved_for_a_project(&home.0);
}

#[test]
fn rows_go_to_their_own_project_and_the_global_orchestrator_stays_global() {
    let project = uuid::Uuid::new_v4().to_string();
    let state: State = serde_json::from_value(serde_json::json!({
        "projects": [{"id": project, "name": "Alpha", "root": "/tmp/alpha", "created_at": 1}],
        "tasks": [{"id": "task-1", "project_id": project, "title": "Ship", "created_at": 1, "updated_at": 1}],
    }))
    .unwrap();
    let terminal = shell("shell-g", ShellKind::Orchestrator, None);
    let work = shell("shell-p", ShellKind::Project, Some(&project));
    let stray = shell("shell-x", ShellKind::Project, None);
    let shells = [terminal, work, stray];
    let global = chat_info(serde_json::json!({"orchestrator": {"scope": "global"}}));
    let orchestrator = chat_info(serde_json::json!({
        "project_id": project,
        "orchestrator": {"scope": "project", "project_id": project},
    }));
    let ordinary = chat_info(serde_json::json!({"project_id": project}));
    let loose = chat_info(serde_json::json!({}));
    let chats = [
        global.clone(),
        orchestrator.clone(),
        ordinary.clone(),
        loose.clone(),
    ];
    let go = |target: Target| route(&target, &state, &shells, Some(&chats));
    let shell_target = |id: &str| Target::Shell { id: id.to_owned() };
    let chat_target = |chat: &ChatInfo| Target::Chat {
        id: chat.id.clone(),
        project_id: chat.project_id.clone(),
        orchestrator: chat.orchestrator.clone(),
    };
    // The global orchestrator, terminal or chat, is this window's Chat view.
    assert_eq!(go(shell_target("shell-g")), Route::GlobalChat);
    assert_eq!(go(chat_target(&global)), Route::GlobalChat);
    // Everything of a project goes to that project's window.
    assert_eq!(
        go(shell_target("shell-p")),
        Route::Project {
            project_id: project.clone(),
            step: Step::Shell("shell-p".into())
        }
    );
    for chat in [&orchestrator, &ordinary] {
        assert_eq!(
            go(chat_target(chat)),
            Route::Project {
                project_id: project.clone(),
                step: Step::Chat(chat.clone())
            }
        );
    }
    assert_eq!(
        go(Target::Project {
            id: project.clone()
        }),
        Route::Project {
            project_id: project.clone(),
            step: Step::Nothing
        }
    );
    assert_eq!(
        go(Target::Task {
            id: "task-1".into()
        }),
        Route::Project {
            project_id: project.clone(),
            step: Step::Task("task-1".into())
        }
    );
    assert_eq!(
        go(Target::Automations {
            project_id: Some(project.clone())
        }),
        Route::Project {
            project_id: project.clone(),
            step: Step::Panel(PanelKind::Schedules)
        }
    );
    // An app automation has no project: the frontmost project window shows it.
    assert_eq!(
        go(Target::Automations { project_id: None }),
        Route::AnyProject(Step::Panel(PanelKind::Schedules))
    );
    // What is gone, or of no project, is said, never made.
    assert!(matches!(go(shell_target("shell-x")), Route::Gone(_)));
    assert!(matches!(go(shell_target("nope")), Route::Gone(_)));
    assert!(matches!(go(chat_target(&loose)), Route::Gone(_)));
    assert!(matches!(
        go(Target::Project { id: "gone".into() }),
        Route::Gone(_)
    ));
    assert!(matches!(
        go(Target::Task { id: "gone".into() }),
        Route::Gone(_)
    ));
    assert!(matches!(
        route(&chat_target(&global), &state, &shells, None),
        Route::Gone(_)
    ));
}

#[gpui::test]
fn a_read_that_began_before_an_orchestrator_was_made_does_not_take_it_away(
    cx: &mut gpui::TestAppContext,
) {
    init_app(cx);
    let home = Home::new();
    let fixture = open_global(
        cx,
        &home.0,
        GlobalWindowState {
            view: GlobalView::Chat,
            ..Default::default()
        },
    );
    let global = fixture.global.clone();
    sample(cx, &fixture, Vec::new(), Vec::new());
    let chat = global_chat(&home.0);
    let begun = global.read_with(cx, |global, _| global.sample_generation);
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        global.update(cx, |global, cx| {
            // An explicit Start made the chat while a read was on its way...
            global.starting = true;
            global.orchestrator_ready(Ok((Orchestrator::Chat(chat.clone()), true)), window, cx);
            assert_eq!(shown_chat(global).map(|(id, _)| id), Some(chat.id.clone()));
            // ...which ends with the chats as they were before it: it is let go.
            global.sampled(begun, Ok((Vec::new(), None, Some(Vec::new()))), window, cx);
            assert_eq!(shown_chat(global).map(|(id, _)| id), Some(chat.id.clone()));
        });
    });
    // A read that began afterwards is taken as it is.
    let now = global.read_with(cx, |global, _| global.sample_generation);
    assert_ne!(now, begun);
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        global.update(cx, |global, cx| {
            global.sampled(
                now,
                Ok((Vec::new(), None, Some(vec![chat.clone()]))),
                window,
                cx,
            );
            assert_eq!(shown_chat(global).map(|(id, _)| id), Some(chat.id.clone()));
            global.sampled(now, Ok((Vec::new(), None, Some(Vec::new()))), window, cx);
            assert!(shown_chat(global).is_none(), "a chat deleted since is gone");
        });
    });
}

#[test]
fn a_reload_reopens_the_window_as_it_handed_it_over_else_as_it_was_last_shown() {
    let home = Home::new();
    let saved = GlobalWindowStore::at(&home.0);
    // Nothing saved: the dashboard.
    assert_eq!(opening_state(None, &saved), GlobalWindowState::default());
    let last = GlobalWindowState {
        view: GlobalView::Chat,
        dashboard_sort: DashboardSort::Name,
        ..GlobalWindowState::default()
    };
    saved.save(&last).unwrap();
    assert_eq!(opening_state(None, &saved), last);
    // A reload's own state wins over the file.
    let handed = GlobalWindowState {
        view: GlobalView::Dashboard,
        show_quiet: true,
        dashboard_filter: Some(DashboardFilter::Working),
        ..GlobalWindowState::default()
    };
    let window = runtime::RuntimeWindow::global(
        &home.0,
        None,
        runtime::WindowMode::Windowed,
        handed.clone(),
    );
    assert_eq!(opening_state(Some(&window), &saved), handed);
}

/// A store with two projects: alpha with a task in progress, and a quiet one.
fn two_projects(home: &Path) -> (Store, String, String) {
    let store = Store::open(home).unwrap();
    let mut ids = Vec::new();
    for name in ["alpha", "quiet"] {
        let root = home.join("work").join(name);
        std::fs::create_dir_all(&root).unwrap();
        ids.push(store.add_project(root, None).unwrap().id);
    }
    let task = store.add_task(&ids[0], "Ship the dashboard", "").unwrap();
    store
        .set_task_status(&task.id, store::TaskStatus::InProgress)
        .unwrap();
    let quiet = ids.pop().unwrap();
    (store, ids.pop().unwrap(), quiet)
}

#[gpui::test]
fn the_dashboards_search_filter_and_order_are_the_windows_own(cx: &mut gpui::TestAppContext) {
    init_app(cx);
    let home = Home::new();
    let (store, alpha, quiet) = two_projects(&home.0);
    let before = store.snapshot().unwrap();
    let fixture = open_global(cx, &home.0, GlobalWindowState::default());
    let global = fixture.global.clone();
    sample(cx, &fixture, Vec::new(), Vec::new());
    let (quiet_row, alpha_row) = (
        format!("dashboard-project-{quiet}"),
        format!("dashboard-project-{alpha}"),
    );
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        // Active is the default: the quiet project waits behind All.
        assert!(window.try_find(alpha_row.clone()).is_some());
        assert!(window.try_find(quiet_row.clone()).is_none());
        window.click("dashboard-filter-all", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        assert!(window.try_find(quiet_row.clone()).is_some());
        window.click("dashboard-sort-name", cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.click("dashboard-search-icon", cx)
    });
    for character in "qui".chars() {
        let character = character.to_string();
        crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
            window.input(&character, cx)
        });
    }
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        let global = global.read(cx);
        assert_eq!(global.query, "qui");
        assert!(window.try_find(alpha_row.clone()).is_none());
        assert!(window.try_find(quiet_row.clone()).is_some());
        assert_eq!(
            (global.filter, global.sort),
            (DashboardFilter::All, DashboardSort::Name)
        );
        // Nothing was started or asked for.
        assert!(!global.starting);
        assert!(matches!(global.shown, Shown::Nothing));
    });
    // The filter and the order are saved as the window's own; the search is not.
    let saved = GlobalWindowStore::at(&home.0).load();
    assert_eq!(
        (saved.filter(), saved.dashboard_sort, saved.show_quiet),
        (DashboardFilter::All, DashboardSort::Name, true)
    );
    let text = std::fs::read_to_string(GlobalWindowStore::at(&home.0).path()).unwrap();
    assert!(!text.contains("qui\""), "{text}");
    let recorded = cx.update(|cx| runtime_windows(cx, true));
    let state = recorded[0].global.clone().unwrap();
    assert_eq!(
        (state.filter(), state.dashboard_sort),
        (DashboardFilter::All, DashboardSort::Name)
    );
    // Searching, filtering and sorting left every project as it was: no selection, no
    // layout, no tabs, no session and no chat.
    assert_eq!(store.snapshot().unwrap(), before);
    nothing_was_started_or_saved_for_a_project(&home.0);

    // Closed and opened again, it lists as it did, with an empty search.
    cx.update_window(fixture.handle, |_, window, _| window.remove_window())
        .unwrap();
    drop(fixture);
    drop(global);
    cx.run_until_parked();
    let saved = opening_state(None, &GlobalWindowStore::at(&home.0));
    let fixture = open_global(cx, &home.0, saved);
    fixture.global.read_with(cx, |global, _| {
        assert_eq!(
            (global.filter, global.sort, global.query.as_str()),
            (DashboardFilter::All, DashboardSort::Name, "")
        );
    });
}

#[gpui::test]
fn the_dashboard_is_laid_out_by_the_windows_own_width(cx: &mut gpui::TestAppContext) {
    init_app(cx);
    let home = Home::new();
    let (_store, _alpha, _quiet) = two_projects(&home.0);
    let fixture = open_global(cx, &home.0, GlobalWindowState::default());
    let global = fixture.global.clone();
    sample(cx, &fixture, Vec::new(), Vec::new());
    for (width, compact, rails) in [
        (800.0, true, &[][..]),
        (1100.0, false, &["dashboard-rail"][..]),
        (
            1900.0,
            false,
            &["dashboard-rail-activity", "dashboard-rail-work"][..],
        ),
    ] {
        cx.simulate_window_resize(fixture.handle, gpui::size(px(width), px(760.0)));
        cx.run_until_parked();
        crate::form_input::test_turn(cx, fixture.handle, |window, _| {
            assert_eq!(
                window.try_find("dashboard-region-projects").is_some(),
                compact,
                "{width}"
            );
            for rail in rails {
                let bounds = window.find(*rail).bounds();
                assert!(bounds.right() <= px(width + 0.5), "{width}: {rail}");
            }
        });
    }
    // ⌘F finds the search on the dashboard.
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        window.dispatch_action(Box::new(FocusSearch), cx);
    });
    crate::form_input::test_turn(cx, fixture.handle, |window, cx| {
        let search = global.read(cx).search.clone();
        assert!(search.read(cx).focus_handle(cx).is_focused(window));
    });
}
