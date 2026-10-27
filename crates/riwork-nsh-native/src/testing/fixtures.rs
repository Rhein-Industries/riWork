//! Acceptance homes generated through the same durable writers as the desktop.
use super::{FakeSession, FakeSessionsFile};
use crate::dto::ShellSessionDto;
use riwork_core::{
    global_window::{DashboardFilter, GlobalView, GlobalWindowState, GlobalWindowStore},
    layouts::{Axis, Layout, LayoutStore, ProjectLayout, SavedPane, SavedTab, WindowSize},
    settings::SettingsStore,
    store::{Project, Store},
};
use riwork_engine::{
    project_tabs::{Kind, Session, Status, TabStore, Update, title_source},
    runtime::{
        RestoreSnapshot, RuntimeManager, RuntimeWindow, WindowGeometry, WindowKind, WindowMode,
    },
    sessions::ShellSession,
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

pub const BETA_CREATE_FAILURE: &str = "Fixture Beta shell creation failed";

#[derive(Debug, Serialize)]
pub struct FixtureManifest {
    pub scenario: String,
    pub root: PathBuf,
    pub home: PathBuf,
    pub runtime: PathBuf,
    pub fake_sessions: PathBuf,
    pub projects: BTreeMap<String, Project>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_ticket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore: Option<RestoreSnapshot>,
}

fn io(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// Refuses existing homes and any path resolving outside an explicit temporary
/// root. UUIDs/timestamps come from production Store; shape, labels and session
/// identities are stable, with generated project IDs recorded in the manifest.
pub fn generate(
    scenario: &str,
    output: &Path,
    executable: Option<&Path>,
) -> Result<FixtureManifest, String> {
    if !matches!(scenario, "p1-basic" | "p1-restore") {
        return Err(format!("Unsupported fixture scenario: {scenario}"));
    }
    if !output.is_absolute()
        || output.components().any(|part| part == Component::ParentDir)
        || output.file_name().is_none_or(|name| name != "home")
    {
        return Err("Fixture output must be an absolute temporary <root>/home path".into());
    }
    let parent = output.parent().ok_or("Fixture needs a parent directory")?;
    let scratch = std::env::temp_dir().canonicalize().map_err(io)?;
    let tmp = Path::new("/tmp").canonicalize().ok();
    // Check the nearest existing ancestor before creating anything (including
    // symlink ancestors); then check the complete root once it exists.
    let ancestor = parent
        .ancestors()
        .find(|path| path.exists())
        .ok_or("Fixture has no existing parent")?
        .canonicalize()
        .map_err(io)?;
    let is_scratch = |path: &Path| {
        path.starts_with(&scratch) || tmp.as_ref().is_some_and(|tmp| path.starts_with(tmp))
    };
    if !is_scratch(&ancestor) {
        return Err("Fixture root must resolve inside a temporary directory".into());
    }
    fs::create_dir_all(parent).map_err(io)?;
    let root = parent.canonicalize().map_err(io)?;
    if !is_scratch(&root)
        || ["home", "repos", "run"]
            .iter()
            .any(|name| root.join(name).symlink_metadata().is_ok())
    {
        return Err("Fixture root must have no existing home, repos or run directory".into());
    }
    if scenario == "p1-restore" {
        let executable = executable.ok_or("p1-restore requires --executable <built riwork>")?;
        if !executable.is_file() {
            return Err(format!(
                "Build riwork first, or pass --executable: {}",
                executable.display()
            ));
        }
    }
    let home = root.join("home");
    let store = Store::open(&home)?;
    let layouts = LayoutStore::open(&home)?;
    let mut projects = BTreeMap::new();
    for name in ["Alpha", "Beta", "Gamma", "Busy"] {
        let repo = root.join("repos").join(name.to_lowercase());
        fs::create_dir_all(&repo).map_err(io)?;
        projects.insert(name.to_owned(), store.add_project(&repo, Some(name))?);
    }
    let alpha = &projects["Alpha"];
    let beta = &projects["Beta"];
    let busy = &projects["Busy"];
    let alpha_layout = navigation_layout();
    let beta_layout = nested_layout();
    let busy_layout = busy_layout();
    layouts.save_chat_state(&alpha.id, &alpha_layout, &[], None)?;
    layouts.save_chat_state(&beta.id, &beta_layout, &[], None)?;
    layouts.save_chat_state(&busy.id, &busy_layout, &[], None)?;
    SettingsStore::open(&home)?.update(|_| {})?;
    GlobalWindowStore::at(&home).save(&GlobalWindowState::default())?;

    let tabs = TabStore::at(&home, &alpha.id)?;
    for (id, kind, title, worker) in [
        (1, Kind::Shell, "Alpha shell 1", false),
        (2, Kind::Shell, "Alpha shell 2", false),
        (3, Kind::Shell, "Alpha shell 3", false),
        (101, Kind::Chat, "Alpha chat", false),
        (5, Kind::Shell, "Hidden worker", true),
    ] {
        let key = format!(
            "{}:{}",
            if kind == Kind::Chat { "chat" } else { "shell" },
            session_id(id)
        );
        tabs.register_created(&Session {
            key: key.clone(),
            kind,
            title: title.into(),
            title_priority: title_source::USER,
            parent_id: None,
            status: Status::Idle,
            orchestrator: false,
            worker,
            created: id,
        })?;
        if worker {
            tabs.update(&Update::Hide { key })?;
        }
    }
    // Deliberately corrupt literal from project_tabs::tests::
    // corruption_is_preserved_and_bad_ids_cannot_escape_home.
    fs::write(
        home.join("project-tabs").join(format!("{}.json", beta.id)),
        b"broken",
    )
    .map_err(io)?;

    let mut sessions = vec![];
    for id in [1, 2, 3, 4, 5, 6, 9] {
        let mut entry = fake_session(id, Some(alpha), &alpha.root, id != 4, false)?;
        if id == 9 {
            entry.appear_after_ms = 300;
        }
        sessions.push(entry);
    }
    sessions.push(fake_session(7, None, &home, true, true)?);
    sessions.push(fake_session(8, Some(beta), &beta.root, true, true)?);
    for id in 301..304 {
        sessions.push(fake_session(id, Some(beta), &beta.root, true, false)?);
    }
    for id in 200..224 {
        sessions.push(fake_session(id, Some(busy), &busy.root, true, false)?);
    }
    let fake_sessions = home.join("fake-sessions.json");
    write_fixture_json(
        &fake_sessions,
        &FakeSessionsFile {
            sessions,
            create_failures: BTreeMap::from([(beta.id.clone(), BETA_CREATE_FAILURE.into())]),
        },
    )?;
    let runtime = root.join("run");
    let restore = if scenario == "p1-restore" {
        let mut global = GlobalWindowState {
            view: GlobalView::Chat,
            ..Default::default()
        };
        global.set_filter(DashboardFilter::NeedsYou);
        global.window_size = WindowSize::new(1100.0, 760.0);
        let windows = vec![
            project_window(
                alpha,
                alpha_layout,
                WindowMode::Windowed,
                false,
                100.0,
                100.0,
            ),
            project_window(beta, beta_layout, WindowMode::Maximized, true, 140.0, 140.0),
            RuntimeWindow::global(
                &home,
                Some(WindowGeometry {
                    x: 180.0,
                    y: 180.0,
                    width: 1100.0,
                    height: 760.0,
                }),
                WindowMode::Windowed,
                global,
            ),
        ];
        Some(RuntimeManager::at(runtime.clone())?.write_fixture_restore(
            &home,
            executable.expect("checked above"),
            windows,
        )?)
    } else {
        None
    };
    let manifest = FixtureManifest {
        scenario: scenario.into(),
        root,
        home,
        runtime,
        fake_sessions,
        projects,
        restore_ticket: restore.as_ref().map(|snapshot| snapshot.ticket_id.clone()),
        restore,
    };
    write_fixture_json(&manifest.home.join("fixture-manifest.json"), &manifest)?;
    Ok(manifest)
}

fn write_fixture_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let mut data = serde_json::to_vec_pretty(value).map_err(io)?;
    data.push(b'\n');
    fs::write(path, data).map_err(io)
}

fn session_id(id: u64) -> String {
    format!("00000000-0000-4000-8000-{id:012}")
}
fn shell(id: u64) -> SavedTab {
    SavedTab::Shell {
        shell_id: session_id(id),
    }
}

pub fn navigation_layout() -> ProjectLayout {
    let mut layout = riwork_nsh_workspace::persistence::default_project_layout();
    layout.locked_panes = None;
    layout.panes.get_mut(&2).unwrap().tabs = vec![
        shell(1),
        shell(2),
        SavedTab::Chat {
            chat_id: session_id(101),
        },
        SavedTab::RemoteShell {
            desktop_id: "fixture-remote".into(),
            shell_id: session_id(102),
        },
    ];
    layout.panes.get_mut(&2).unwrap().active_tab_key = Some(shell(2).key());
    layout.normalize().expect("valid L-NAV fixture");
    layout
}

pub fn nested_layout() -> ProjectLayout {
    let mut layout = riwork_nsh_workspace::persistence::default_project_layout();
    layout.layout = Layout::Split {
        axis: Axis::SideBySide,
        ratio: 0.30,
        first: Box::new(Layout::Pane(1)),
        second: Box::new(Layout::Split {
            axis: Axis::Stacked,
            ratio: 0.62,
            first: Box::new(Layout::Pane(2)),
            second: Box::new(Layout::Split {
                axis: Axis::Stacked,
                ratio: 0.50,
                first: Box::new(Layout::Pane(3)),
                second: Box::new(Layout::Pane(4)),
            }),
        }),
    };
    for (pane, id) in [(2, 301), (3, 302), (4, 303)] {
        layout.panes.insert(
            pane,
            SavedPane {
                tabs: vec![shell(id)],
                active_tab_key: Some(shell(id).key()),
                ..Default::default()
            },
        );
    }
    layout.active_pane = 3;
    layout.main_pane = Some(2);
    layout.normalize().expect("valid L-NESTED fixture");
    layout
}

fn busy_layout() -> ProjectLayout {
    let mut layout = riwork_nsh_workspace::persistence::default_project_layout();
    layout.panes.get_mut(&2).unwrap().tabs = (200..224).map(shell).collect();
    layout.panes.get_mut(&2).unwrap().active_tab_key = Some(shell(200).key());
    layout.normalize().expect("valid busy fixture");
    layout
}

fn fake_session(
    id: u64,
    project: Option<&Project>,
    cwd: &Path,
    alive: bool,
    orchestrator: bool,
) -> Result<FakeSession, String> {
    let session: ShellSession = serde_json::from_value(serde_json::json!({
        "id": session_id(id), "project_id": project.map(|project| &project.id),
        "worktree_id": null, "kind": if orchestrator { "orchestrator" } else { "project" },
        "cwd": cwd, "command": null, "created_at_unix": id, "user_opened": id != 5,
    }))
    .map_err(io)?;
    let mut session = ShellSessionDto::from(session);
    session.alive = alive;
    Ok(FakeSession {
        session,
        appear_after_ms: 0,
    })
}

fn project_window(
    project: &Project,
    layout: ProjectLayout,
    mode: WindowMode,
    focus_mode: bool,
    x: f32,
    y: f32,
) -> RuntimeWindow {
    RuntimeWindow {
        project_id: Some(project.id.clone()),
        path: project.root.clone(),
        bounds: Some(WindowGeometry {
            x,
            y,
            width: 1000.0,
            height: 700.0,
        }),
        mode,
        layout: Some(layout),
        focus_mode,
        focus_centered: false,
        kind: WindowKind::Project,
        global: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backend::SessionsBackend,
        testing::{FakeSessions, ManualClock},
    };
    use std::time::Duration;

    #[test]
    fn basic_fixture_uses_production_readers_and_fake_clock_without_processes() {
        let root = tempfile::tempdir().unwrap();
        let manifest = generate("p1-basic", &root.path().join("home"), None).unwrap();
        let state = Store::open(&manifest.home).unwrap().snapshot().unwrap();
        assert_eq!(state.projects.len(), 4);
        assert_eq!(
            state.active_project_id.as_deref(),
            Some(manifest.projects["Alpha"].id.as_str())
        );
        let layouts = LayoutStore::open(&manifest.home).unwrap();
        assert_eq!(
            layouts
                .load(&manifest.projects["Alpha"].id)
                .unwrap()
                .unwrap(),
            navigation_layout()
        );
        assert_eq!(
            layouts
                .load(&manifest.projects["Beta"].id)
                .unwrap()
                .unwrap(),
            nested_layout()
        );
        assert!(
            layouts
                .load(&manifest.projects["Gamma"].id)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            layouts
                .load(&manifest.projects["Busy"].id)
                .unwrap()
                .unwrap()
                .panes[&2]
                .tabs
                .len(),
            24
        );
        let tabs = TabStore::at(&manifest.home, &manifest.projects["Alpha"].id)
            .unwrap()
            .list()
            .unwrap();
        assert_eq!(tabs.len(), 5);
        assert_eq!(tabs.iter().filter(|tab| tab.hidden).count(), 1);
        assert!(
            TabStore::at(&manifest.home, &manifest.projects["Beta"].id)
                .unwrap()
                .list()
                .is_err()
        );
        let data = serde_json::from_slice(&fs::read(&manifest.fake_sessions).unwrap()).unwrap();
        let clock = ManualClock::default();
        let sessions = FakeSessions::new(data, clock.clone());
        assert_eq!(sessions.list().unwrap().sessions.len(), 35);
        clock.advance(Duration::from_millis(299));
        assert_eq!(sessions.list().unwrap().sessions.len(), 35);
        clock.advance(Duration::from_millis(1));
        assert_eq!(sessions.list().unwrap().sessions.len(), 36);
        assert_eq!(
            sessions.create_failure(&manifest.projects["Beta"].id),
            Some(BETA_CREATE_FAILURE)
        );
        assert!(generate("p1-basic", &manifest.home, None).is_err());
        assert!(!manifest.home.join("appearance.json").exists());
        assert!(!manifest.home.join("global-window").exists());
    }

    #[test]
    fn restore_fixture_is_a_private_production_ticket_for_three_windows() {
        let root = tempfile::tempdir().unwrap();
        let manifest = generate(
            "p1-restore",
            &root.path().join("home"),
            Some(&std::env::current_exe().unwrap()),
        )
        .unwrap();
        let snapshot = manifest.restore.as_ref().unwrap();
        let path = manifest
            .runtime
            .join("restores")
            .join(format!("{}.json", snapshot.ticket_id));
        let saved: RestoreSnapshot = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.windows, snapshot.windows);
        assert_eq!(saved.windows.len(), 3);
        assert_eq!(
            saved.windows[0].bounds.unwrap(),
            WindowGeometry {
                x: 100.0,
                y: 100.0,
                width: 1000.0,
                height: 700.0
            }
        );
        assert_eq!(saved.windows[1].mode, WindowMode::Maximized);
        assert!(saved.windows[1].focus_mode);
        assert!(saved.windows[2].is_global());
        assert_eq!(
            saved.windows[2].global.as_ref().unwrap().view,
            GlobalView::Chat
        );
        assert_eq!(
            saved.windows[2].global.as_ref().unwrap().filter(),
            DashboardFilter::NeedsYou
        );
        assert!(
            RuntimeManager::at(manifest.runtime.clone())
                .unwrap()
                .instances()
                .unwrap()
                .is_empty()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
