//! Blocking adapters for workspace sessions and shared membership. No JS IO.
use riwork_core::{
    settings::{SettingsStore, TabCloseBehavior},
    store::Store,
};
use riwork_engine::{
    project_tabs::{self, Kind, Session, Snapshot, Status, TabStore, Update},
    sessions::{SessionManager, ShellKind},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

/// The existing process home watcher calls this; layouts never start a watcher.
pub fn changed() {
    let _ = changes().send(());
}
pub fn changes() -> &'static tokio::sync::broadcast::Sender<()> {
    static CHANNEL: OnceLock<tokio::sync::broadcast::Sender<()>> = OnceLock::new();
    CHANNEL.get_or_init(|| tokio::sync::broadcast::channel(16).0)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Shell {
    pub id: String,
    #[serde(default, alias = "projectId")]
    pub project_id: Option<String>,
    #[serde(default, alias = "worktreeId")]
    pub worktree_id: Option<String>,
    #[serde(default = "project_kind")]
    pub kind: String,
    #[serde(default)]
    pub cwd: PathBuf,
    #[serde(default)]
    pub alive: bool,
    #[serde(default, alias = "userOpened")]
    pub user_opened: bool,
    #[serde(default, alias = "parentId")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: Option<Status>,
    #[serde(default)]
    pub appear_after_ms: u64,
}
fn project_kind() -> String {
    "project".into()
}
impl Shell {
    pub fn worker(&self) -> bool {
        self.kind != "orchestrator" && (!self.user_opened || self.parent_id.is_some())
    }
    fn inventory(&self) -> Session {
        Session {
            key: format!("shell:{}", self.id),
            kind: Kind::Shell,
            title: if self.title.is_empty() {
                "Shell".into()
            } else {
                self.title.clone()
            },
            title_priority: 0,
            parent_id: self.parent_id.clone(),
            status: if self.alive {
                self.status.clone().unwrap_or(Status::Idle)
            } else {
                Status::Stopped
            },
            orchestrator: self.kind == "orchestrator",
            worker: self.worker(),
            created: 1,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct FakeFile {
    #[serde(default)]
    sessions: Vec<Shell>,
    #[serde(default)]
    chats: Vec<FakeChat>,
    #[serde(default)]
    create_errors: BTreeMap<String, String>,
    #[serde(default)]
    list_errors: BTreeMap<String, String>,
    #[serde(default)]
    close_calls: Vec<String>,
    #[serde(default)]
    global_chat_ids: std::collections::HashSet<String>,
}
#[derive(Serialize, Deserialize)]
struct FakeChat {
    id: String,
    #[serde(default, alias = "projectId")]
    project_id: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default, alias = "parentId")]
    parent_id: Option<String>,
    #[serde(default)]
    status: Option<Status>,
}
impl FakeChat {
    fn inventory(&self) -> Session {
        Session {
            key: format!("chat:{}", self.id),
            kind: Kind::Chat,
            title: self.title.clone(),
            title_priority: 0,
            parent_id: self.parent_id.clone(),
            status: self.status.clone().unwrap_or(Status::Idle),
            orchestrator: false,
            worker: self.parent_id.is_some(),
            created: 1,
        }
    }
}
static FAKE_LOCK: Mutex<()> = Mutex::new(());
static FIXTURES: OnceLock<Mutex<std::collections::BTreeSet<PathBuf>>> = OnceLock::new();
#[cfg(debug_assertions)]
pub fn register_fixture(home: &Path) -> Result<(), String> {
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    if !home.starts_with(
        Path::new("/tmp")
            .canonicalize()
            .map_err(|e| e.to_string())?,
    ) {
        return Err("invalid: fixtures must be isolated".into());
    }
    FIXTURES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(home);
    Ok(())
}
fn fake_path(home: &Path) -> Option<PathBuf> {
    // A test fixture is an explicit opt-in, restricted to disposable homes.
    if !cfg!(debug_assertions) && !cfg!(feature = "test-support") {
        return None;
    }
    let registered = FIXTURES.get().is_some_and(|homes| {
        homes
            .lock()
            .unwrap()
            .contains(&home.canonicalize().unwrap_or_default())
    });
    let file = if registered {
        home.join("fake-sessions.json")
    } else {
        PathBuf::from(std::env::var_os("RIWORK_NSH_FAKE_SESSIONS")?)
    };
    let canonical = file.canonicalize().ok()?;
    let scratch = Path::new("/tmp").canonicalize().ok()?;
    (canonical.starts_with(scratch) && canonical.starts_with(home.canonicalize().ok()?))
        .then_some(canonical)
}
fn read_fake(path: &Path) -> Result<FakeFile, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn write_fake(path: &Path, value: &FakeFile) -> Result<(), String> {
    std::fs::write(
        path,
        serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    changed();
    Ok(())
}
fn fake_elapsed(path: &Path, rows: &[Shell]) -> Duration {
    static STARTS: OnceLock<Mutex<BTreeMap<PathBuf, Instant>>> = OnceLock::new();
    let mut starts = STARTS.get_or_init(Default::default).lock().unwrap();
    let start = *starts.entry(path.to_owned()).or_insert_with(|| {
        // One shot per delayed fixture delivery. There is no recurring timer.
        for delay in rows
            .iter()
            .map(|s| s.appear_after_ms)
            .filter(|d| *d > 0)
            .collect::<std::collections::BTreeSet<_>>()
        {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                changed();
            });
        }
        Instant::now()
    });
    start.elapsed()
}
pub fn list(home: &Path) -> Result<Vec<Shell>, String> {
    if let Some(path) = fake_path(home) {
        let _guard = FAKE_LOCK.lock().unwrap();
        let file = read_fake(&path)?;
        let elapsed = fake_elapsed(&path, &file.sessions).as_millis() as u64;
        return Ok(file
            .sessions
            .into_iter()
            .filter(|s| s.appear_after_ms <= elapsed)
            .collect());
    }
    Ok(SessionManager::at(home.to_owned())?
        .list()?
        .into_iter()
        .map(|s| Shell {
            id: s.id,
            project_id: s.project_id,
            worktree_id: s.worktree_id,
            kind: if s.kind == ShellKind::Project {
                "project"
            } else {
                "orchestrator"
            }
            .into(),
            cwd: s.cwd,
            alive: s.alive,
            user_opened: s.user_opened,
            parent_id: s.parent_id,
            title: s.command.unwrap_or_else(|| "Shell".into()),
            status: None,
            appear_after_ms: 0,
        })
        .collect())
}

pub struct Data {
    pub project_id: String,
    pub root: Option<PathBuf>,
    pub shells: Option<Vec<Shell>>,
    pub tabs: Option<Snapshot>,
    pub close_behavior: TabCloseBehavior,
    pub theme: riwork_core::theme::ThemeChoice,
    pub ui_text_size: f32,
    pub panel_tab_icons: bool,
    pub notice: Option<String>,
    pub global_chats: std::collections::HashSet<String>,
    pub owners: BTreeMap<String, String>,
}
pub fn load(home: &Path, project: &str) -> Result<Data, String> {
    let state = Store::open(home.to_owned())?.snapshot()?;
    let root = state
        .projects
        .iter()
        .find(|p| p.id == project)
        .map(|p| p.root.clone());
    let settings = SettingsStore::open(home)?.load()?;
    let close_behavior = settings.tab_close_behavior;
    let panel_tab_icons = root.is_some() && settings.panel_tab_icons;
    let theme = if root.is_some() {
        settings.theme
    } else {
        riwork_core::theme::ThemeChoice::Native
    };
    let mut data = Data {
        project_id: project.into(),
        root,
        shells: None,
        tabs: None,
        close_behavior,
        theme,
        ui_text_size: settings.ui_text_size.points(),
        panel_tab_icons,
        notice: None,
        global_chats: Default::default(),
        owners: BTreeMap::new(),
    };
    // Restore-only / read-only consumers without a registered local project do
    // not touch the shell backend, and cannot accidentally launch a session.
    if data.root.is_none() {
        return Ok(data);
    }
    let fake = fake_path(home)
        .map(|path| {
            let _guard = FAKE_LOCK.lock().unwrap();
            read_fake(&path)
        })
        .transpose()?;
    if let Some(fake) = &fake {
        data.global_chats = fake.global_chat_ids.clone();
        for chat in &fake.chats {
            if let Some(owner) = &chat.project_id {
                data.owners
                    .insert(format!("chat:{}", chat.id), owner.clone());
            } else {
                data.global_chats.insert(chat.id.clone());
            }
        }
        if let Some(error) = fake.list_errors.get(project) {
            data.notice = Some(error.clone());
            return Ok(data);
        }
    }
    if fake.is_none() {
        for chat in riwork_engine::chat::log::read_infos(home) {
            if let Some(project) = chat.project_id {
                data.owners.insert(format!("chat:{}", chat.id), project);
            } else {
                data.global_chats.insert(chat.id);
            }
        }
    }
    match list(home) {
        Ok(shells) => {
            for shell in &shells {
                if let Some(project) = &shell.project_id {
                    data.owners
                        .insert(format!("shell:{}", shell.id), project.clone());
                }
            }
            let tabs = if let Some(fake) = &fake {
                TabStore::at(home, project).and_then(|store| {
                    let inventory = shells
                        .iter()
                        .filter(|s| s.project_id.as_deref() == Some(project))
                        .map(Shell::inventory)
                        .chain(
                            fake.chats
                                .iter()
                                .filter(|c| c.project_id.as_deref() == Some(project))
                                .map(FakeChat::inventory),
                        )
                        .collect::<Vec<_>>();
                    store.reconcile(&inventory)?;
                    project_tabs::snapshot_for(&store, &inventory)
                })
            } else {
                project_tabs::refresh_snapshot_with_activity(home, project, &BTreeMap::new())
            };
            // An unreadable list retains the classic per-pane strip.
            data.tabs = tabs.ok();
            data.shells = Some(shells);
        }
        Err(error) => data.notice = Some(error),
    }
    Ok(data)
}

pub fn create(
    home: &Path,
    project: &str,
    cwd: Option<PathBuf>,
    worktree: Option<String>,
) -> Result<Shell, String> {
    let state = Store::open(home.to_owned())?.snapshot()?;
    let root = state.project(project)?.root.clone();
    let cwd = cwd
        .or_else(|| {
            worktree.as_ref().and_then(|id| {
                state
                    .worktrees
                    .iter()
                    .find(|w| &w.id == id && w.project_id == project)
                    .map(|w| w.path.clone())
            })
        })
        .unwrap_or(root);
    if let Some(path) = fake_path(home) {
        let _guard = FAKE_LOCK.lock().unwrap();
        let mut file = read_fake(&path)?;
        if let Some(error) = file.create_errors.get(project) {
            return Err(error.clone());
        }
        let shell = Shell {
            id: uuid::Uuid::new_v4().to_string(),
            project_id: Some(project.into()),
            worktree_id: worktree,
            kind: "project".into(),
            cwd,
            alive: true,
            user_opened: true,
            parent_id: None,
            title: "Shell".into(),
            status: Some(Status::Idle),
            appear_after_ms: 0,
        };
        file.sessions.push(shell.clone());
        write_fake(&path, &file)?;
        TabStore::at(home, project)?.register_created(&shell.inventory())?;
        return Ok(shell);
    }
    let shell = SessionManager::at(home.to_owned())?.create(project.into(), worktree, cwd, None)?;
    changed();
    Ok(Shell {
        id: shell.id,
        project_id: shell.project_id,
        worktree_id: shell.worktree_id,
        kind: "project".into(),
        cwd: shell.cwd,
        alive: true,
        user_opened: true,
        parent_id: None,
        title: "Shell".into(),
        status: Some(Status::Idle),
        appear_after_ms: 0,
    })
}
pub fn close(home: &Path, id: &str) -> Result<(), String> {
    if let Some(path) = fake_path(home) {
        let _guard = FAKE_LOCK.lock().unwrap();
        let mut file = read_fake(&path)?;
        if let Some(shell) = file.sessions.iter_mut().find(|s| s.id == id) {
            shell.alive = false;
        }
        file.close_calls.push(id.into());
        return write_fake(&path, &file);
    }
    SessionManager::at(home.to_owned())?.close(id)?;
    changed();
    Ok(())
}
pub fn update(home: &Path, project: &str, update: Update) -> Result<(), String> {
    TabStore::at(home, project)?.update(&update)?;
    changed();
    Ok(())
}

pub fn exit_key(home: &Path, key: &str) -> Result<(), String> {
    if let Some(id) = key.strip_prefix("shell:") {
        return close(home, id);
    }
    let id = key.strip_prefix("chat:").ok_or("invalid: session key")?;
    if let Some(path) = fake_path(home) {
        let _guard = FAKE_LOCK.lock().unwrap();
        let mut file = read_fake(&path)?;
        file.close_calls.push(key.into());
        return write_fake(&path, &file);
    }
    let socket = riwork_engine::chat::client::socket_path(home);
    if !socket.exists() {
        return Ok(());
    }
    riwork_engine::chat::client::Client::connect(&socket)?
        .command(id, riwork_engine::chat::model::ChatCommand::Stop)
}
