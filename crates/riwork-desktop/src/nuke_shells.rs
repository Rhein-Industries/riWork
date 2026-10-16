//! **Nuke all shells**: a labeled button in the Shells panel that exits every live
//! shell of the window's project, tabbed or detached, workers included, after one
//! confirmation. Each shell ends through the single-tab Exit's own call (`exit_session`);
//! a tab goes only once its shell has exited, so a shell that would not exit keeps its tab
//! and is named in the notice. Orchestrator terminals keep their own lifecycle, chats are
//! not shells, and other projects' and other Macs' sessions are never this window's to end.

use super::*;
use crate::controls::Button;
use gpui::{EventEmitter, FocusHandle, Render};

/// The id of the button in each visible Shells panel.
pub(crate) const NUKE_BUTTON: &str = "nuke-shells";

/// The shells the button exits: this project's live terminals (`shared_shell_adoptable`),
/// whether a tab shows them or not and whoever started them. An orchestrator's terminal is
/// another kind of shell, and an ended one has nothing to stop.
pub(crate) fn nuke_scope(shells: &[ShellSession], project_id: &str) -> Vec<String> {
    shells
        .iter()
        .filter(|shell| shared_shell_adoptable(shell, project_id))
        .map(|shell| shell.id.clone())
        .collect()
}

/// "1 shell", "7 shells".
fn shells_word(count: usize) -> String {
    match count {
        1 => "1 shell".to_owned(),
        count => format!("{count} shells"),
    }
}

/// What the dialog says will happen.
pub(crate) fn nuke_body(count: usize) -> String {
    format!(
        "Exit {} in this project. Running processes and agents stop and unsaved terminal \
         output is lost. Orchestrator shells and chats are kept.",
        shells_word(count)
    )
}

/// The confirm button, which names what it ends.
pub(crate) fn nuke_confirm_label(count: usize) -> String {
    format!("Exit {}", shells_word(count))
}

/// What the window says afterwards. Every shell that did not exit is named with its error;
/// a partial run never reads as a success.
pub(crate) fn nuke_notice(exited: usize, failed: &[(String, String)]) -> String {
    if failed.is_empty() {
        return format!("Exited {}.", shells_word(exited));
    }
    let names = failed
        .iter()
        .map(|(name, error)| format!("{name} ({error})"))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Exited {exited} of {}. Not exited, tabs kept: {names}",
        shells_word(exited + failed.len())
    )
}

/// The one confirmation. Cancel has the focus, so Return and Space cancel too; Escape
/// cancels; only the confirm button exits anything, clicked or focused and pressed. Emits
/// `true` for that button.
pub struct NukeShellsDialog {
    focus: FocusHandle,
    buttons: [FocusHandle; 2],
    handle: gpui_kit::base::DialogHandle,
    count: usize,
}

impl EventEmitter<bool> for NukeShellsDialog {}

impl NukeShellsDialog {
    pub fn new(count: usize, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            buttons: [cx.focus_handle(), cx.focus_handle()],
            handle: gpui_kit::base::DialogHandle::new(true),
            count,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.buttons[0].focus(window, cx);
    }
}

impl Render for NukeShellsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        let error = theme::diff_colors(cx).removed;
        let panel = div()
            .id("nuke-shells-dialog")
            // Pinned Base Dialog's focus-trap host omits its AX role.
            .role(gpui::Role::AlertDialog)
            .aria_label("Nuke all shells")
            .occlude()
            .w(ui_text::space(380.0))
            .max_w_full()
            .p(ui_text::space(18.0))
            .flex()
            .flex_col()
            .gap(ui_text::space(10.0))
            .font_family(ui_text::ui_family())
            .text_size(ui_text::text(10.0))
            .text_color(rgb(colors.text))
            .bg(rgb(colors.panel))
            .border_1()
            .border_color(rgb(colors.magenta))
            // Native: a sheet with rounded corners, a hairline and a shadow, as Close is.
            .map(|dialog| {
                controls::native(dialog, |dialog| {
                    dialog
                        .rounded(controls::radius(12.0))
                        .border_color(rgb(colors.divider))
                        .shadow_lg()
                })
            })
            .child(
                div()
                    .text_size(ui_text::text(13.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Nuke all shells?"),
            )
            .child(
                div()
                    .id("nuke-shells-body")
                    .text_color(rgb(colors.muted))
                    .child(nuke_body(self.count)),
            )
            .child(
                div()
                    .pt(ui_text::space(6.0))
                    .flex()
                    .justify_end()
                    .gap(ui_text::space(8.0))
                    .children(
                        [
                            ("Cancel".to_owned(), false),
                            (nuke_confirm_label(self.count), true),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(index, (label, confirm))| {
                            behavior_controls::button(
                                ("nuke-shells-action", index),
                                label,
                                Button::Secondary,
                                colors,
                            )
                            .track_focus(&self.buttons[index])
                            .when(confirm, |button| button.text_color(rgb(error)))
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(confirm)))
                        }),
                    ),
            );
        let cancel = cx.listener(|_, _: &gpui::ClickEvent, _, cx| cx.emit(false));
        // The dialog takes Return before its buttons do: it presses the focused one, which
        // is Cancel unless the confirm button was tabbed to.
        let ok = cx.listener(|dialog, _: &gpui::ClickEvent, window, cx| {
            cx.emit(dialog.buttons[1].is_focused(window));
        });
        gpui_kit::base::Dialog::new(cx)
            .handle(self.handle.clone())
            .focus_handle(self.focus.clone())
            .close_on_backdrop_press(false)
            .on_ok(move |event, window, cx| {
                ok(event, window, cx);
                true
            })
            .on_cancel(move |event, window, cx| {
                cancel(event, window, cx);
                true
            })
            .popup(panel)
    }
}

impl Workspace {
    /// The shells the button would exit now. A project on another Mac has none here.
    pub(crate) fn nuke_targets(&self) -> Vec<String> {
        if self.is_remote() {
            return Vec::new();
        }
        nuke_scope(&self.shells, &self.project_id)
    }

    /// Open the confirmation for the shells there are now. Nothing ends until it is
    /// confirmed, and only these shells can: one created while it is open is not counted.
    pub(crate) fn request_nuke_shells(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_open() || self.nuke_running {
            return;
        }
        let targets = self.nuke_targets();
        if targets.is_empty() {
            return;
        }
        // Native terminals draw above the window's content; freeze them as the other
        // modals do, or the dialog is hidden under the terminal of the selected tab.
        self.strip_menu = None;
        self.begin_tab_drag(cx);
        let dialog = cx.new(|cx| NukeShellsDialog::new(targets.len(), cx));
        dialog.update(cx, |dialog, cx| dialog.focus(window, cx));
        self.nuke_target = Some(targets);
        cx.subscribe_in(&dialog, window, |workspace, _, confirmed, window, cx| {
            workspace.nuke_dialog = None;
            workspace.finish_tab_drag(cx);
            // Taken once: a second confirm from the same dialog finds nothing to exit.
            if let Some(targets) = workspace.nuke_target.take()
                && *confirmed
            {
                workspace.nuke_shells(targets, window, cx);
            }
            workspace.focus_active(window, cx);
            cx.notify();
        })
        .detach();
        self.nuke_dialog = Some(dialog);
        cx.notify();
    }

    /// Exit the confirmed shells that are still live shells of the project. One that ended
    /// while the dialog was open needs no exit; one created meanwhile was never confirmed.
    fn nuke_shells(&mut self, confirmed: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.nuke_running {
            return;
        }
        let live = self.nuke_targets().into_iter().collect::<HashSet<_>>();
        let targets = confirmed
            .into_iter()
            .filter(|id| live.contains(id))
            .collect::<Vec<_>>();
        if targets.is_empty() {
            self.notice = Some("No shells to exit: they had already ended.".into());
            cx.notify();
            return;
        }
        let names = targets
            .iter()
            .map(|id| (id.clone(), self.nuke_shell_name(id)))
            .collect::<HashMap<_, _>>();
        self.nuke_running = true;
        let exit = self.session_exit.clone();
        let sessions = self.sessions.clone();
        let work = cx.background_executor().spawn(async move {
            targets
                .into_iter()
                .map(|id| {
                    let result = exit(&sessions, &format!("shell:{id}"));
                    (id, result)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn_in(window, async move |this, cx| {
            let results = work.await;
            let _ = this.update_in(cx, |workspace, window, cx| {
                workspace.finish_nuke(results, &names, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_nuke(
        &mut self,
        results: Vec<(String, Result<(), String>)>,
        names: &HashMap<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.nuke_running = false;
        let mut exited = HashSet::new();
        let mut failed = Vec::new();
        for (id, result) in results {
            match result {
                Ok(()) => {
                    exited.insert(id);
                }
                Err(error) => {
                    let name = names.get(&id).cloned().unwrap_or_else(|| id.clone());
                    failed.push((name, error));
                }
            }
        }
        self.drop_exited_shells(&exited, window, cx);
        self.notice = Some(nuke_notice(exited.len(), &failed));
        self.session_refresh_generation = self.session_refresh_generation.wrapping_add(1);
        cx.notify();
    }

    /// Close the tabs of shells that have exited, and forget them until the next refresh
    /// reads the sessions again. Exit already removed their entries from the shared list
    /// (`SessionManager::close`); the window's copy follows, and the strip drops their tabs
    /// as it does any removed entry's, choosing the selected tab as it always does.
    fn drop_exited_shells(
        &mut self,
        exited: &HashSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if exited.is_empty() {
            return;
        }
        self.shells.retain(|shell| !exited.contains(&shell.id));
        let keys = exited
            .iter()
            .map(|id| format!("shell:{id}"))
            .collect::<HashSet<_>>();
        if let Some(entries) = &mut self.shared_tabs {
            entries.retain(|entry| !keys.contains(&entry.key));
            self.shared_tab_keys.extend(keys);
            self.apply_shared_tabs(window, cx);
        } else {
            let tabs = self
                .panes
                .iter()
                .flat_map(|(pane_id, pane)| {
                    pane.tabs
                        .iter()
                        .filter(|tab| tab.shell_id().is_some_and(|id| exited.contains(id)))
                        .map(|tab| (*pane_id, tab.id))
                })
                .collect::<Vec<_>>();
            for (pane_id, tab_id) in tabs {
                self.remove_tab(pane_id, tab_id, window, cx);
            }
        }
        // Closing a tab without a shared list detaches its shell; these have ended instead.
        self.detached_shell_ids.retain(|id| !exited.contains(id));
    }

    /// How the notice names a shell: its tab's title, else its short id.
    fn nuke_shell_name(&self, id: &str) -> String {
        let key = format!("shell:{id}");
        self.strip_entry(&key)
            .map(|entry| entry.title.clone())
            .or_else(|| {
                self.panes
                    .values()
                    .flat_map(|pane| &pane.tabs)
                    .find(|tab| tab.shell_id() == Some(id))
                    .map(|tab| tab.title.clone())
            })
            .unwrap_or_else(|| format!("shell {}", remote_tree::short_id(id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use std::sync::Mutex;

    const CONFIRM: (&str, usize) = ("nuke-shells-action", 1);
    const CANCEL: (&str, usize) = ("nuke-shells-action", 0);

    /// The sessions of the fixture project and around it.
    struct Ids {
        /// This project's shells with a tab in the window.
        tabbed: [String; 2],
        /// One the user detached: its entry is hidden and no tab shows it.
        detached: String,
        /// A worker an orchestrator started from the CLI: no tab and no entry.
        headless: String,
        /// This project's, but its session has ended.
        ended: String,
        /// This project's orchestrator, with a tab.
        orchestrator: String,
        /// The global orchestrator's terminal, which every local project sees.
        global: String,
        /// A live shell of another project.
        other: String,
        /// A chat of this project, with a tab.
        chat: String,
    }

    impl Ids {
        fn in_scope(&self) -> Vec<String> {
            let mut ids = vec![
                self.tabbed[0].clone(),
                self.tabbed[1].clone(),
                self.detached.clone(),
                self.headless.clone(),
            ];
            ids.sort();
            ids
        }
    }

    struct Fixture {
        handle: gpui::AnyWindowHandle,
        workspace: Entity<Workspace>,
        home: PathBuf,
        ids: Ids,
        /// The keys the recording Exit was asked to end, in order.
        exits: Arc<Mutex<Vec<String>>>,
        /// The keys it refuses to end.
        refuse: Arc<Mutex<HashSet<String>>>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    impl Fixture {
        fn turn(&self, cx: &mut gpui::TestAppContext, action: impl FnOnce(&mut Window, &mut App)) {
            crate::form_input::test_turn(cx, self.handle, action);
        }

        fn read<R>(&self, cx: &mut gpui::TestAppContext, read: impl FnOnce(&Workspace) -> R) -> R {
            self.workspace.read_with(cx, |workspace, _| read(workspace))
        }

        fn exits(&self) -> Vec<String> {
            self.exits.lock().unwrap().clone()
        }

        fn tab_keys(&self, cx: &mut gpui::TestAppContext) -> Vec<String> {
            self.read(cx, |workspace| {
                workspace.panes[&1]
                    .tabs
                    .iter()
                    .filter_map(session_tab_key)
                    .collect()
            })
        }

        fn open_dialog(&self, cx: &mut gpui::TestAppContext) {
            self.turn(cx, |window, cx| window.click(NUKE_BUTTON, cx));
            assert!(self.read(cx, |workspace| workspace.nuke_dialog.is_some()));
        }
    }

    fn shell_tab(id: TabId, shell: &str, title: &str) -> Tab {
        Tab {
            id,
            title: title.to_owned(),
            content: TabContent::Shell {
                shell_id: shell.to_owned(),
                worktree_id: None,
                terminal: None,
                // Never attach: a fixture has no tmux session behind its tabs.
                attach_error: Some("fixture".to_owned()),
                attach_failures: 0,
            },
            hidden_since: None,
        }
    }

    fn entry(
        key: &str,
        kind: project_tabs::Kind,
        title: &str,
        hidden: bool,
        order: usize,
    ) -> project_tabs::Entry {
        project_tabs::Entry {
            key: key.to_owned(),
            kind,
            title: title.to_owned(),
            base_title: title.to_owned(),
            title_priority: 0,
            status: project_tabs::Status::Idle,
            hidden,
            worker: false,
            legacy_user_opened: false,
            legacy_dismissed: false,
            order,
            parent: None,
            children: Vec::new(),
            child_count: 0,
            rename: None,
            created: 0,
        }
    }

    /// A local project's window, laid out as if loaded so nothing asks tmux, with Exit
    /// replaced by a recording fake. One pane holds Shells, two of the project's terminals, its
    /// orchestrator and a chat. With `shells` false the project has none of its own shells,
    /// only the sessions the button must leave alone.
    fn fixture(cx: &mut gpui::TestAppContext, shells: bool) -> Fixture {
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
        let home = std::env::temp_dir().join(format!("rw-nuke-{}", uuid::Uuid::new_v4()));
        let project: Project = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "name": "Fixture",
            "root": home,
            "created_at": 1,
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
        let id = || uuid::Uuid::new_v4().to_string();
        let ids = Ids {
            tabbed: [id(), id()],
            detached: id(),
            headless: id(),
            ended: id(),
            orchestrator: id(),
            global: id(),
            other: id(),
            chat: id(),
        };
        let shell = crate::global_window::tests::shell;
        let ours = Some(project_id.as_str());
        let mut sessions = vec![
            shell(&ids.orchestrator, ShellKind::Orchestrator, ours),
            shell(&ids.global, ShellKind::Orchestrator, None),
            shell(&ids.other, ShellKind::Project, Some(&id())),
            ShellSession {
                alive: false,
                ..shell(&ids.ended, ShellKind::Project, ours)
            },
        ];
        if shells {
            for tabbed in &ids.tabbed {
                sessions.push(ShellSession {
                    user_opened: true,
                    ..shell(tabbed, ShellKind::Project, ours)
                });
            }
            sessions.push(ShellSession {
                user_opened: true,
                ..shell(&ids.detached, ShellKind::Project, ours)
            });
            sessions.push(ShellSession {
                parent_id: Some(ids.orchestrator.clone()),
                ..shell(&ids.headless, ShellKind::Project, ours)
            });
        }
        let chat = serde_json::from_value::<ChatInfo>(serde_json::json!({
            "id": ids.chat,
            "provider": "codex",
            "project_id": project_id,
            "cwd": "/tmp",
            "title": "Chat",
            "created_at_unix": 1,
            "state": {"state": "waiting"},
        }))
        .unwrap();
        let mut tabs = vec![
            Tab {
                id: 1,
                title: "SESSIONS".to_owned(),
                content: TabContent::Panel(PanelKind::Shells),
                hidden_since: None,
            },
            shell_tab(4, &ids.orchestrator, "P·ORCH"),
        ];
        let mut entries = vec![
            entry(
                &format!("shell:{}", ids.orchestrator),
                project_tabs::Kind::Shell,
                "P·ORCH",
                false,
                0,
            ),
            entry(
                &format!("chat:{}", ids.chat),
                project_tabs::Kind::Chat,
                "Chat",
                false,
                1,
            ),
        ];
        if shells {
            tabs.insert(1, shell_tab(2, &ids.tabbed[0], "zsh 01"));
            tabs.insert(2, shell_tab(3, &ids.tabbed[1], "zsh 02"));
            for (order, (shell, title, hidden)) in [
                (&ids.tabbed[0], "zsh 01", false),
                (&ids.tabbed[1], "zsh 02", false),
                (&ids.detached, "zsh 03", true),
            ]
            .into_iter()
            .enumerate()
            {
                entries.push(entry(
                    &format!("shell:{shell}"),
                    project_tabs::Kind::Shell,
                    title,
                    hidden,
                    order + 2,
                ));
            }
        }
        let exits = Arc::new(Mutex::new(Vec::new()));
        let refuse = Arc::new(Mutex::new(HashSet::new()));
        let (record, refused) = (exits.clone(), refuse.clone());
        let session_exit: SessionExit = Arc::new(move |_: &SessionManager, key: &str| {
            record.lock().unwrap().push(key.to_owned());
            if refused.lock().unwrap().contains(key) {
                Err("tmux timed out".to_owned())
            } else {
                Ok(())
            }
        });
        let (handle, workspace) = crate::form_input::test_window(cx, move |window, cx| {
            Workspace::build(startup, None, window, cx, false)
        });
        let chat_id = ids.chat.clone();
        crate::form_input::test_turn(cx, handle, |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.session_exit = session_exit;
                workspace.shells = sessions;
                workspace.panes = BTreeMap::from([(1, Pane { tabs, active: 0 })]);
                workspace.layout = Layout::Pane(1);
                workspace.active_pane = 1;
                workspace.main_pane = Some(1);
                workspace.locked_panes = Some(HashSet::new());
                workspace.next_tab_id = 10;
                workspace.shared_chats = Some(vec![chat]);
                workspace.shared_tab_keys = entries.iter().map(|e| e.key.clone()).collect();
                workspace.shared_tabs = Some(entries);
                // Adopts the chat's tab.
                workspace.apply_shared_tabs(window, cx);
                assert!(chat_tab_in(&workspace.panes, &chat_id).is_some());
            });
        });
        Fixture {
            handle,
            workspace,
            home,
            ids,
            exits,
            refuse,
        }
    }

    #[test]
    fn the_copy_names_the_count_and_what_is_kept() {
        assert_eq!(
            nuke_body(7),
            "Exit 7 shells in this project. Running processes and agents stop and unsaved \
             terminal output is lost. Orchestrator shells and chats are kept."
        );
        assert!(nuke_body(1).starts_with("Exit 1 shell in this project."));
        assert_eq!(nuke_confirm_label(7), "Exit 7 shells");
        assert_eq!(nuke_confirm_label(1), "Exit 1 shell");
        assert_eq!(nuke_notice(3, &[]), "Exited 3 shells.");
        let failed = [("zsh 02".to_owned(), "tmux timed out".to_owned())];
        assert_eq!(
            nuke_notice(2, &failed),
            "Exited 2 of 3 shells. Not exited, tabs kept: zsh 02 (tmux timed out)"
        );
        assert!(nuke_notice(0, &failed).starts_with("Exited 0 of 1 shell."));
    }

    #[gpui::test]
    fn with_no_shells_of_its_own_the_button_is_shown_disabled_and_opens_nothing(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = fixture(cx, false);
        assert!(fixture.read(cx, |workspace| workspace.nuke_targets().is_empty()));
        fixture.turn(cx, |window, cx| {
            let button = crate::form_input::test_ax_node(window, cx, NUKE_BUTTON);
            assert_eq!(button.role(), gpui::Role::Button);
            assert_eq!(button.label(), Some("Nuke all shells"));
            assert!(button.is_disabled());
            assert!(!button.supports_action(gpui::accesskit::Action::Click));
            window.click(NUKE_BUTTON, cx);
        });
        assert!(fixture.read(cx, |workspace| workspace.nuke_dialog.is_none()));
        // Not even a direct request opens it.
        fixture.turn(cx, |window, cx| {
            fixture.workspace.update(cx, |workspace, cx| {
                workspace.request_nuke_shells(window, cx);
                assert!(workspace.nuke_dialog.is_none());
            });
        });
        assert!(fixture.exits().is_empty());
    }

    #[gpui::test]
    fn the_dialog_counts_the_projects_tabbed_and_detached_shells_and_focuses_cancel(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = fixture(cx, true);
        let mut scope = fixture.read(cx, |workspace| workspace.nuke_targets());
        scope.sort();
        // Both tabbed shells, the detached one and the headless worker; not the orchestrators,
        // the ended shell, the other project's shell or the chat.
        assert_eq!(scope, fixture.ids.in_scope());
        fixture.turn(cx, |window, cx| {
            let button = crate::form_input::test_ax_node(window, cx, NUKE_BUTTON);
            assert!(!button.is_disabled());
            assert_eq!(button.label(), Some("Nuke all shells"));
        });
        fixture.open_dialog(cx);
        fixture.turn(cx, |window, cx| {
            assert_eq!(
                window.find(CONFIRM).label().as_deref(),
                Some("Exit 4 shells")
            );
            assert_eq!(window.find(CANCEL).label().as_deref(), Some("Cancel"));
            let dialog = fixture
                .workspace
                .read(cx)
                .nuke_dialog
                .clone()
                .expect("the dialog is open");
            let dialog = dialog.read(cx);
            assert_eq!(dialog.count, 4);
            assert!(
                dialog.buttons[0].is_focused(window),
                "Cancel is the default"
            );
            assert!(
                fixture.workspace.read(cx).tab_dragging,
                "terminals are frozen"
            );
        });
        assert!(
            fixture.exits().is_empty(),
            "nothing ends before confirmation"
        );
    }

    #[gpui::test]
    fn cancel_escape_and_return_on_cancel_exit_nothing(cx: &mut gpui::TestAppContext) {
        let fixture = fixture(cx, true);
        let before = fixture.tab_keys(cx);
        fixture.open_dialog(cx);
        fixture.turn(cx, |window, cx| window.click(CANCEL, cx));
        assert!(fixture.read(cx, |workspace| workspace.nuke_dialog.is_none()
            && workspace.nuke_target.is_none()
            && !workspace.tab_dragging));
        fixture.open_dialog(cx);
        fixture.turn(cx, |window, cx| window.press("escape", cx));
        assert!(fixture.read(cx, |workspace| workspace.nuke_dialog.is_none()));
        for key in ["enter", "space"] {
            fixture.open_dialog(cx);
            fixture.turn(cx, |window, cx| window.press(key, cx));
            assert!(
                fixture.read(cx, |workspace| workspace.nuke_dialog.is_none()),
                "{key} presses the focused Cancel"
            );
        }
        assert!(fixture.exits().is_empty());
        assert_eq!(fixture.tab_keys(cx), before);
        assert_eq!(
            fixture.read(cx, |workspace| workspace.nuke_targets().len()),
            4
        );
    }

    #[gpui::test]
    fn return_on_the_tabbed_to_confirm_button_exits(cx: &mut gpui::TestAppContext) {
        let fixture = fixture(cx, true);
        fixture.open_dialog(cx);
        fixture.turn(cx, |window, cx| window.press("tab", cx));
        fixture.turn(cx, |window, cx| {
            let dialog = fixture.workspace.read(cx).nuke_dialog.clone().unwrap();
            assert!(dialog.read(cx).buttons[1].is_focused(window));
            window.press("enter", cx);
        });
        assert_eq!(fixture.exits().len(), 4);
        assert!(fixture.read(cx, |workspace| workspace.nuke_dialog.is_none()));
    }

    #[gpui::test]
    fn confirming_exits_exactly_the_projects_shells_and_closes_their_tabs(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = fixture(cx, true);
        fixture.open_dialog(cx);
        fixture.turn(cx, |window, cx| window.click(CONFIRM, cx));
        let mut exits = fixture.exits();
        exits.sort();
        let expected = fixture
            .ids
            .in_scope()
            .iter()
            .map(|id| format!("shell:{id}"))
            .collect::<Vec<_>>();
        assert_eq!(
            exits, expected,
            "each shell once, through Exit, and nothing else"
        );
        let ids = &fixture.ids;
        // The exited shells' tabs closed; the Shells panel, orchestrator and chat stay.
        assert_eq!(
            fixture.tab_keys(cx),
            vec![
                format!("shell:{}", ids.orchestrator),
                format!("chat:{}", ids.chat),
            ]
        );
        fixture.read(cx, |workspace| {
            let pane = &workspace.panes[&1];
            assert_eq!(pane.tabs[pane.active].id, 1, "Shells stays selected");
            let keys = workspace
                .shared_tab_entries()
                .iter()
                .map(|e| e.key.clone())
                .collect::<HashSet<_>>();
            assert!(keys.contains(&format!("chat:{}", ids.chat)));
            assert!(keys.contains(&format!("shell:{}", ids.orchestrator)));
            assert!(!keys.contains(&format!("shell:{}", ids.detached)));
            let left = workspace
                .shells
                .iter()
                .map(|shell| shell.id.clone())
                .collect::<HashSet<_>>();
            for id in [&ids.orchestrator, &ids.global, &ids.other, &ids.ended] {
                assert!(left.contains(id));
            }
            assert!(workspace.nuke_targets().is_empty());
            assert!(!workspace.nuke_running);
            assert_eq!(workspace.notice.as_deref(), Some("Exited 4 shells."));
        });
        fixture.turn(cx, |window, cx| {
            let button = crate::form_input::test_ax_node(window, cx, NUKE_BUTTON);
            assert!(button.is_disabled(), "nothing is left to exit");
        });
    }

    #[gpui::test]
    fn a_shell_that_fails_to_exit_keeps_its_tab_and_is_named(cx: &mut gpui::TestAppContext) {
        let fixture = fixture(cx, true);
        let failing = format!("shell:{}", fixture.ids.tabbed[1]);
        fixture.refuse.lock().unwrap().insert(failing.clone());
        fixture.open_dialog(cx);
        fixture.turn(cx, |window, cx| window.click(CONFIRM, cx));
        assert_eq!(fixture.exits().len(), 4);
        let keys = fixture.tab_keys(cx);
        assert!(keys.contains(&failing), "its tab stays");
        assert!(!keys.contains(&format!("shell:{}", fixture.ids.tabbed[0])));
        fixture.read(cx, |workspace| {
            assert_eq!(
                workspace.notice.as_deref(),
                Some("Exited 3 of 4 shells. Not exited, tabs kept: zsh 02 (tmux timed out)")
            );
            assert!(
                workspace
                    .shared_tab_entries()
                    .iter()
                    .any(|e| e.key == failing && !e.hidden)
            );
            assert_eq!(
                workspace.nuke_targets(),
                vec![fixture.ids.tabbed[1].clone()]
            );
        });
        fixture.turn(cx, |window, cx| {
            let button = crate::form_input::test_ax_node(window, cx, NUKE_BUTTON);
            assert!(
                !button.is_disabled(),
                "the shell that is left can be tried again"
            );
        });
    }

    #[gpui::test]
    fn a_second_confirm_or_click_cannot_run_twice(cx: &mut gpui::TestAppContext) {
        let fixture = fixture(cx, true);
        fixture.open_dialog(cx);
        // Confirm twice before anything else runs: the click lands on the same button twice.
        cx.update_window(fixture.handle, |_, window, cx| {
            window.render_frame(cx);
            window.double_click(CONFIRM, cx);
        })
        .unwrap();
        // The exits are queued, not yet done: the button is off and another request waits.
        cx.update_window(fixture.handle, |_, window, cx| {
            fixture.workspace.update(cx, |workspace, cx| {
                assert!(workspace.nuke_running);
                assert!(workspace.nuke_dialog.is_none());
                workspace.request_nuke_shells(window, cx);
                assert!(
                    workspace.nuke_dialog.is_none(),
                    "no second dialog while running"
                );
                workspace.nuke_shells(fixture.ids.in_scope(), window, cx);
            });
            window.render_frame(cx);
            let button = crate::form_input::test_ax_node(window, cx, NUKE_BUTTON);
            assert!(button.is_disabled());
        })
        .unwrap();
        cx.run_until_parked();
        let mut exits = fixture.exits();
        exits.sort();
        let expected = fixture
            .ids
            .in_scope()
            .iter()
            .map(|id| format!("shell:{id}"))
            .collect::<Vec<_>>();
        assert_eq!(exits, expected, "each shell exited exactly once");
        assert!(!fixture.read(cx, |workspace| workspace.nuke_running));
    }

    #[gpui::test]
    fn only_the_confirmed_shells_that_are_still_live_are_exited(cx: &mut gpui::TestAppContext) {
        let fixture = fixture(cx, true);
        fixture.open_dialog(cx);
        let late = uuid::Uuid::new_v4().to_string();
        let ended = fixture.ids.headless.clone();
        fixture.workspace.update(cx, |workspace, _| {
            let project = workspace.project_id.clone();
            // A shell starts while the dialog is open, and the headless worker ends.
            workspace.shells.push(crate::global_window::tests::shell(
                &late,
                ShellKind::Project,
                Some(&project),
            ));
            for shell in &mut workspace.shells {
                if shell.id == ended {
                    shell.alive = false;
                }
            }
        });
        fixture.turn(cx, |window, cx| window.click(CONFIRM, cx));
        let exits = fixture.exits();
        assert_eq!(exits.len(), 3);
        assert!(!exits.contains(&format!("shell:{late}")), "never confirmed");
        assert!(!exits.contains(&format!("shell:{ended}")), "already ended");
        fixture.read(cx, |workspace| {
            assert_eq!(workspace.notice.as_deref(), Some("Exited 3 shells."));
            assert_eq!(workspace.nuke_targets(), vec![late.clone()]);
        });
    }

    #[gpui::test]
    fn the_labeled_button_stays_inside_shells_in_a_split_or_focused_pane(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = fixture(cx, true);
        fixture.turn(cx, |_, cx| {
            fixture.workspace.update(cx, |workspace, cx| {
                // Shells remains in the left pane; the top-right pane has no Shells panel.
                workspace.panes.insert(
                    2,
                    Pane {
                        tabs: Vec::new(),
                        active: 0,
                    },
                );
                workspace.layout = Layout::Split {
                    axis: Axis::SideBySide,
                    ratio: 0.5,
                    first: Box::new(Layout::Pane(1)),
                    second: Box::new(Layout::Pane(2)),
                };
                cx.notify();
            });
        });
        fixture.turn(cx, |window, cx| {
            let button = window.within("sessions-controls").find(NUKE_BUTTON);
            assert_eq!(button.label().as_deref(), Some("Nuke all shells"));
            let button = button.bounds();
            let left_menu = window.find("pane-1-menu").bounds();
            let right_menu = window.find("pane-2-menu").bounds();
            let refresh = window.find("sessions-refresh").bounds();
            assert!(
                button.top() > left_menu.bottom(),
                "{button:?} below {left_menu:?}"
            );
            assert!(
                button.right() < right_menu.left(),
                "{button:?} stays in Shells' left pane"
            );
            assert!(
                button.top() >= refresh.top(),
                "{button:?} belongs to the sessions toolbar"
            );
            fixture.workspace.update(cx, |workspace, cx| {
                workspace.focus_mode = true;
                workspace.active_pane = 1;
                cx.notify();
            });
        });
        fixture.turn(cx, |window, cx| {
            let bounds = window.find("sessions-controls").bounds();
            let button = window
                .within("sessions-controls")
                .find(NUKE_BUTTON)
                .bounds();
            assert!(
                button.top() >= bounds.top() && button.bottom() <= bounds.bottom(),
                "{button:?} remains inside the Shells toolbar {bounds:?} in focus mode"
            );
            window.click(NUKE_BUTTON, cx);
        });
        assert!(fixture.read(cx, |workspace| workspace.nuke_dialog.is_some()));
        assert!(fixture.exits().is_empty());
    }

    #[gpui::test]
    fn a_stale_shells_panel_cannot_nuke_another_project_or_generation(
        cx: &mut gpui::TestAppContext,
    ) {
        let fixture = fixture(cx, true);
        fixture.turn(cx, |window, cx| {
            fixture.workspace.update(cx, |workspace, cx| {
                for (project, generation) in [
                    (
                        "another-project".to_owned(),
                        workspace.native_sessions_generation,
                    ),
                    (
                        workspace.project_id.clone(),
                        workspace.native_sessions_generation + 1,
                    ),
                ] {
                    workspace.panel_action(
                        PanelAction::NukeShells {
                            project,
                            generation,
                        },
                        window,
                        cx,
                    );
                    assert!(workspace.nuke_dialog.is_none());
                }
            });
        });
        assert!(fixture.exits().is_empty());
    }
}
