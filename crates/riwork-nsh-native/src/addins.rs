//! Add-ins in the app (riwork_core::addins): the ones a window's project can use, trusting
//! them, their settings, new ones from the template, their builds, and the add-in API their
//! engines call (`addin.*` host calls, served here). The engine itself is the `riwork.addin`
//! native view (addin_view.rs).
//!
//! The API answers only what the add-in's manifest was granted; everything that changes what
//! the person sees (a shell tab, a file, another view, a notification, a status item) goes to
//! the add-in's tab in RiWork as an action, and RiWork does it there.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

use nsh_core::prelude::*;
use nsh_engine::{HostCall, HostChannel};
use riwork_core::addins::{self, Found, Manifest, Permission, Scope};
use riwork_engine::{addin_build, addin_worker::WorkerProcess};
use serde_json::{Value, json};

use crate::{Subscription, hub::Hub, service};

/// The add-in API's own runtime: its calls come from add-in engine threads, outside NSH's.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("riwork-addin-api")
            .enable_all()
            .build()
            .expect("the add-in API runtime starts")
    })
}

/// A project's id and root, from the store.
fn project(home: &Path, project_id: &str) -> Result<Option<(String, PathBuf)>, String> {
    if project_id.is_empty() {
        return Ok(None);
    }
    let state = riwork_core::store::Store::open(home)?.snapshot()?;
    Ok(state
        .projects
        .into_iter()
        .find(|project| project.id == project_id)
        .map(|project| (project.id, project.root)))
}

fn discover(home: &Path, project_id: &str) -> Result<Vec<Found>, String> {
    let project = project(home, project_id)?;
    Ok(addins::discover(home, project.as_ref().map(|(id, root)| (id.as_str(), root.as_path()))))
}

/// One add-in, by key, as a window of `project_id` sees it.
pub(crate) fn find(home: &Path, project_id: &str, key: &str) -> Result<Found, String> {
    discover(home, project_id)?
        .into_iter()
        .find(|found| found.key() == key)
        .ok_or_else(|| format!("The add-in {key} is not there any more."))
}

fn manifest(found: &Found) -> Result<&Manifest, String> {
    found.manifest.as_ref().map_err(Clone::clone)
}

/// The folder an installed package keeps its prebuilt bundle in (crate addin_package).
const PREBUILT: &str = ".bundle";

/// The bundle to run: an installed package's prebuilt one, else a build of its source.
pub(crate) fn bundle(home: &Path, found: &Found) -> Result<(PathBuf, String), String> {
    let manifest = manifest(found)?;
    let prebuilt = found.dir.join(PREBUILT).join("bundle.js");
    if prebuilt.is_file() && !found.dir.join("App.tsx").is_file() {
        let version = format!("package-{}", manifest.version);
        return Ok((prebuilt, version));
    }
    addin_build::build(home, &found.scope, &found.dir, manifest).map(|built| (built.bundle, built.digest))
}

pub(crate) fn grant(found: &Found) -> Option<String> {
    found.manifest.as_ref().ok().map(|manifest| addins::grant_digest(&found.dir, manifest))
}

pub(crate) fn is_trusted(home: &Path, found: &Found) -> bool {
    grant(found).is_some_and(|digest| addins::trusted(home, &found.key(), &digest))
}

/// What the Add-ins panel shows of one.
fn describe(home: &Path, found: &Found) -> Value {
    let scope = match found.scope {
        Scope::Project { .. } => "project",
        Scope::Personal => "personal",
    };
    let base = json!({
        "key": found.key(), "scope": scope, "dir": found.dir.display().to_string(),
        "installed": found.dir.join(".package.json").is_file(),
    });
    let Ok(manifest) = &found.manifest else {
        let problem = found.manifest.as_ref().err().cloned();
        return json!({"key": base["key"], "scope": scope, "dir": base["dir"], "problem": problem,
            "name": found.dir.file_name().map(|name| name.to_string_lossy().into_owned()), "trusted": false});
    };
    let data = addins::data_dir(home, &found.scope, &manifest.id);
    let values = addins::settings(&data, manifest);
    json!({
        "key": base["key"], "scope": scope, "dir": base["dir"], "installed": base["installed"],
        "id": manifest.id, "name": manifest.name, "version": manifest.version, "description": manifest.description,
        "views": manifest.views,
        "permissions": manifest.permissions.iter().map(|permission| json!({
            "id": serde_json::to_value(permission).unwrap(), "text": permission.describe()})).collect::<Vec<_>>(),
        "network": manifest.network,
        "workers": manifest.workers.iter().map(|worker| json!({"id": worker.id, "description": worker.description})).collect::<Vec<_>>(),
        "tools": manifest.tools.iter().map(|tool| json!({"name": tool.name, "description": tool.description})).collect::<Vec<_>>(),
        "commands": manifest.commands,
        "settings": manifest.settings.iter().map(|(key, spec)| json!({
            "key": key, "label": spec.label, "type": spec.kind, "description": spec.description,
            "value": values.get(key).cloned().unwrap_or(Value::Null)})).collect::<Vec<_>>(),
        "trusted": is_trusted(home, found),
        "problem": Value::Null,
    })
}

// MARK: Changes

/// Published whenever add-ins appear, change or are trusted, so lists and tabs look again.
fn changes() -> &'static Hub<Value> {
    static CHANGES: OnceLock<Hub<Value>> = OnceLock::new();
    CHANGES.get_or_init(|| Hub::new(json!({"generation": 0})))
}

fn changed() {
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    changes().publish(json!({"generation": generation}));
}

/// Folders watched for add-ins coming and going (the personal one, and each project's that a
/// list was asked for).
fn watch_folders(paths: &[PathBuf]) {
    use notify::Watcher;
    static WATCH: OnceLock<Mutex<Option<(notify::RecommendedWatcher, Vec<PathBuf>)>>> = OnceLock::new();
    let mut slot = WATCH.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if slot.is_none() {
        let watcher = notify::recommended_watcher(|event: notify::Result<notify::Event>| {
            if event.is_ok_and(|event| !event.paths.iter().any(|path| path.components().any(|part| part.as_os_str() == ".bundle" || part.as_os_str() == "node_modules"))) {
                changed();
            }
        });
        match watcher {
            Ok(watcher) => *slot = Some((watcher, Vec::new())),
            Err(_) => return,
        }
    }
    let (watcher, watched) = slot.as_mut().unwrap();
    for path in paths {
        if watched.contains(path) {
            continue;
        }
        // Watch the nearest folder that exists: `.ontack/addins` appears with the first add-in.
        let existing = path.ancestors().find(|ancestor| ancestor.is_dir()).map(Path::to_path_buf);
        if let Some(existing) = existing {
            if watcher.watch(&existing, notify::RecursiveMode::Recursive).is_ok() {
                watched.push(path.clone());
            }
        }
    }
}

// MARK: The add-in API

/// One running add-in view: what it may do, and how to reach its tab and its engine.
pub(crate) struct Api {
    pub home: PathBuf,
    pub key: String,
    pub scope: Scope,
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub project_id: String,
    /// To the add-in's tab in RiWork: actions the person sees.
    pub emit: Box<dyn Fn(&str, Value) + Send + Sync>,
    /// To the add-in's engine: events.
    pub channel: HostChannel,
    workers: Mutex<HashMap<String, Arc<WorkerProcess>>>,
}

fn live() -> &'static Mutex<Vec<Weak<Api>>> {
    static LIVE: OnceLock<Mutex<Vec<Weak<Api>>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}

const CALLS: [(&str, Option<Permission>); 15] = [
    ("addin.project", Some(Permission::ProjectRead)),
    ("addin.files.open", Some(Permission::ProjectRead)),
    ("addin.tasks.list", Some(Permission::ProjectRead)),
    ("addin.tasks.add", Some(Permission::TasksWrite)),
    ("addin.tasks.status", Some(Permission::TasksWrite)),
    ("addin.shell.run", Some(Permission::ShellRun)),
    ("addin.notify", Some(Permission::Notify)),
    ("addin.status", Some(Permission::Status)),
    ("addin.storage.get", None),
    ("addin.storage.set", None),
    ("addin.settings", None),
    ("addin.views.open", None),
    ("addin.worker.start", None),
    ("addin.worker.request", None),
    ("addin.worker.stop", None),
];

impl Api {
    pub fn new(
        home: PathBuf,
        found: &Found,
        project_id: String,
        channel: HostChannel,
        emit: Box<dyn Fn(&str, Value) + Send + Sync>,
    ) -> Result<Arc<Self>, String> {
        let manifest = manifest(found)?.clone();
        let api = Arc::new(Self {
            home, key: found.key(), scope: found.scope.clone(), dir: found.dir.clone(), manifest,
            project_id, emit, channel, workers: Mutex::default(),
        });
        let mut live = live().lock().unwrap();
        live.retain(|api| api.strong_count() > 0);
        live.push(Arc::downgrade(&api));
        Ok(api)
    }

    /// Answers the add-in API on the engine's channel. Each call checks the permission it
    /// needs, then runs off the engine thread.
    pub fn serve(self: &Arc<Self>) {
        for (name, permission) in CALLS {
            let api = Arc::downgrade(self);
            self.channel.handle(name, move |mut call: HostCall| {
                let Some(api) = api.upgrade() else { return call.reject("The add-in's view has closed.") };
                if let Some(permission) = permission.filter(|permission| !api.manifest.allows(*permission)) {
                    let id = serde_json::to_value(permission).unwrap();
                    return call.reject(format!("{} does not have the {} permission in its addin.json.", api.manifest.name, id.as_str().unwrap_or("")));
                }
                let args = call.take_data();
                runtime().spawn(async move {
                    let result = api.call(name, args).await;
                    call.reply(result);
                });
            });
        }
    }

    fn data(&self) -> PathBuf {
        addins::data_dir(&self.home, &self.scope, &self.manifest.id)
    }

    async fn snapshot(&self) -> Result<crate::dto::StoreState, String> {
        service::initialize().await?.store_snapshot().await
    }

    async fn project(&self) -> Result<(crate::dto::ProjectDto, crate::dto::StoreState), String> {
        let state = self.snapshot().await?;
        let project = state
            .projects
            .iter()
            .find(|project| project.id == self.project_id)
            .cloned()
            .ok_or("This add-in's tab is not in a project window.")?;
        Ok((project, state))
    }

    /// A path of the project (relative to its root, or inside it or one of its worktrees).
    async fn project_path(&self, path: &str) -> Result<PathBuf, String> {
        let (project, state) = self.project().await?;
        let root = PathBuf::from(&project.root);
        let candidate = if Path::new(path).is_absolute() { PathBuf::from(path) } else { root.join(path) };
        let resolved = candidate.canonicalize().map_err(|_| format!("{path} does not exist"))?;
        let allowed = std::iter::once(root)
            .chain(state.worktrees.iter().filter(|tree| tree.project_id == project.id).map(|tree| PathBuf::from(&tree.path)))
            .filter_map(|dir| dir.canonicalize().ok())
            .any(|dir| resolved.starts_with(dir));
        allowed.then_some(resolved).ok_or_else(|| format!("{path} is not in the project"))
    }

    async fn call(self: Arc<Self>, name: &str, args: Value) -> Result<Value, String> {
        let text = |field: &str| args.get(field).and_then(Value::as_str).unwrap_or_default().to_owned();
        match name {
            "addin.project" => {
                let (project, state) = self.project().await?;
                let worktrees: Vec<Value> = state.worktrees.iter()
                    .filter(|tree| tree.project_id == project.id && (tree.is_primary || tree.present))
                    .map(|tree| json!({"id": tree.id, "branch": tree.branch, "path": tree.path, "primary": tree.is_primary}))
                    .collect();
                Ok(json!({"id": project.id, "name": project.name, "root": project.root, "worktrees": worktrees}))
            }
            "addin.tasks.list" => {
                let (project, state) = self.project().await?;
                Ok(json!(state.tasks.iter().filter(|task| task.project_id == project.id).map(|task| json!({
                    "id": task.id, "title": task.title, "details": task.details, "status": task.status, "worktreeId": task.worktree_id,
                })).collect::<Vec<_>>()))
            }
            "addin.tasks.add" => {
                let title = text("title");
                if title.trim().is_empty() || title.len() > 500 {
                    return Err("A task's title is 1 to 500 characters.".into());
                }
                let task = service::initialize().await?.add_task(self.project_id.clone(), title, text("details")).await?;
                Ok(json!({"id": task.id, "title": task.title, "details": task.details, "status": task.status, "worktreeId": task.worktree_id}))
            }
            "addin.tasks.status" => {
                let id = text("id");
                let (project, state) = self.project().await?;
                if !state.tasks.iter().any(|task| task.id == id && task.project_id == project.id) {
                    return Err("That task is not this project's.".into());
                }
                let task = service::initialize().await?.set_task_status(id, text("status")).await?;
                Ok(json!({"id": task.id, "title": task.title, "details": task.details, "status": task.status, "worktreeId": task.worktree_id}))
            }
            "addin.files.open" => {
                let path = self.project_path(&text("path")).await?;
                (self.emit)("action", json!({"type": "file", "path": path}));
                Ok(Value::Null)
            }
            "addin.shell.run" => {
                let command = text("command");
                if command.trim().is_empty() || command.len() > 8000 {
                    return Err("A command is 1 to 8000 characters.".into());
                }
                let (project, state) = self.project().await?;
                let worktree = text("worktree");
                let cwd = if worktree.is_empty() {
                    project.root.clone()
                } else {
                    state.worktrees.iter().find(|tree| tree.id == worktree && tree.project_id == project.id)
                        .map(|tree| tree.path.clone()).ok_or("That worktree is not this project's.")?
                };
                (self.emit)("action", json!({"type": "shell", "command": command, "cwd": cwd,
                    "worktree": (!worktree.is_empty()).then_some(worktree), "title": text("title")}));
                Ok(Value::Null)
            }
            "addin.notify" => {
                (self.emit)("action", json!({"type": "notify", "title": text("title"), "body": text("body")}));
                Ok(Value::Null)
            }
            "addin.status" => {
                let tone = match text("tone").as_str() { "warning" => "warning", "critical" => "critical", _ => "normal" };
                let status = args.get("text").and_then(Value::as_str).map(|value| value.chars().take(60).collect::<String>());
                (self.emit)("action", json!({"type": "status", "text": status, "tone": tone}));
                Ok(Value::Null)
            }
            "addin.storage.get" => Ok(addins::read_storage(&self.data()).remove(&text("key")).unwrap_or(Value::Null)),
            "addin.storage.set" => {
                let value = args.get("value").cloned().filter(|value| !value.is_null());
                addins::write_storage(&self.data(), &text("key"), value).map(|_| Value::Null)
            }
            "addin.settings" => Ok(Value::Object(addins::settings(&self.data(), &self.manifest))),
            "addin.views.open" => {
                let addin = args.get("addin").and_then(Value::as_str).filter(|id| !id.is_empty());
                let (key, view_title) = match addin {
                    None => (self.key.clone(), self.manifest.views.iter().find(|view| view.id == text("view")).map(|view| view.title.clone())),
                    // Another add-in, by id: the window resolves it, and its trust applies there.
                    Some(id) => (format!("id:{id}"), None),
                };
                if addin.is_none() && view_title.is_none() {
                    return Err(format!("{} has no view {:?}.", self.manifest.name, text("view")));
                }
                (self.emit)("action", json!({"type": "view", "addin": key, "view": text("view"),
                    "title": view_title, "args": args.get("args").cloned().unwrap_or(Value::Null)}));
                Ok(Value::Null)
            }
            "addin.worker.start" => self.start_worker(&text("id"), &args),
            "addin.worker.request" => {
                let worker = self.workers.lock().unwrap().get(&text("worker")).cloned()
                    .ok_or("That worker has stopped.")?;
                let method = text("method");
                let params = args.get("params").cloned().unwrap_or(Value::Null);
                tokio::task::spawn_blocking(move || worker.request(&method, params, Duration::from_secs(600)))
                    .await
                    .map_err(|error| error.to_string())?
            }
            "addin.worker.stop" => {
                if let Some(worker) = self.workers.lock().unwrap().remove(&text("worker")) {
                    worker.stop();
                }
                Ok(Value::Null)
            }
            _ => Err(format!("unknown add-in call {name}")),
        }
    }

    fn start_worker(self: &Arc<Self>, id: &str, args: &Value) -> Result<Value, String> {
        let spec = self.manifest.workers.iter().find(|worker| worker.id == id)
            .ok_or_else(|| format!("{} declares no worker {id:?}.", self.manifest.name))?;
        let extra: Vec<String> = args.get("args").and_then(Value::as_array)
            .map(|values| values.iter().filter_map(|value| value.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        let handle = uuid::Uuid::new_v4().to_string();
        let log = self.data().join(format!("worker-{id}.log"));
        std::fs::create_dir_all(self.data()).map_err(|error| error.to_string())?;
        let channel = self.channel.clone();
        let worker_handle = handle.clone();
        let process = WorkerProcess::start(&self.dir, spec, &extra, &log, move |event, data| {
            let _ = channel.send("addin.worker", json!({"worker": worker_handle, "event": event, "data": data}));
        })?;
        self.workers.lock().unwrap().insert(handle.clone(), Arc::new(process));
        Ok(json!({"worker": handle}))
    }

    /// The view closed: its workers stop with it.
    pub fn close(&self) {
        for (_, worker) in self.workers.lock().unwrap().drain() {
            worker.stop();
        }
        for name in CALLS.map(|(name, _)| name) {
            self.channel.remove_handler(name);
        }
    }
}

// MARK: JavaScript

fn home() -> Result<PathBuf, String> {
    service::process_home()
}

fn list(project_id: &str) -> Result<Value, String> {
    let home = home()?;
    let project = project(&home, project_id)?;
    let mut folders = vec![addins::personal_dir(&home)];
    if let Some((_, root)) = &project {
        folders.push(addins::project_dir(root));
    }
    watch_folders(&folders);
    let found = discover(&home, project_id)?;
    Ok(json!(found.iter().map(|found| describe(&home, found)).collect::<Vec<_>>()))
}

fn notify_settings(key: &str, home: &Path) {
    for api in live().lock().unwrap().iter().filter_map(Weak::upgrade) {
        if api.key == key && api.home == home {
            let _ = api.channel.send("addin.settings", Value::Object(addins::settings(&api.data(), &api.manifest)));
        }
    }
}

#[module]
pub struct Addins;
#[module]
impl Addins {
    /// The add-ins a window of `project_id` can use: `[{key, id, name, version, description,
    /// scope, dir, views, permissions: [{id, text}], network, workers, tools, commands,
    /// settings: [{key, label, type, description, value}], trusted, installed, problem}]`.
    pub async fn list(project_id: String) -> Result<Value, String> {
        service::blocking(move || list(&project_id)).await
    }
    /// Trusts an add-in with what its manifest asks for now; a later change asks again.
    pub async fn trust(project_id: String, key: String) -> Result<Value, String> {
        service::blocking(move || {
            let home = home()?;
            let found = find(&home, &project_id, &key)?;
            let digest = grant(&found).ok_or("This add-in's addin.json cannot be read.")?;
            addins::trust(&home, &key, &digest)?;
            changed();
            list(&project_id)
        })
        .await
    }
    pub async fn revoke(project_id: String, key: String) -> Result<Value, String> {
        service::blocking(move || {
            addins::revoke(&home()?, &key)?;
            changed();
            list(&project_id)
        })
        .await
    }
    pub async fn set_setting(project_id: String, key: String, name: String, value: Value) -> Result<Value, String> {
        service::blocking(move || {
            let home = home()?;
            let found = find(&home, &project_id, &key)?;
            let manifest = manifest(&found)?;
            addins::set_setting(&addins::data_dir(&home, &found.scope, &manifest.id), manifest, &name, value)?;
            notify_settings(&key, &home);
            list(&project_id)
        })
        .await
    }
    /// A new add-in from the template, in the project (`.ontack/addins`) or the person's own.
    pub async fn create(project_id: String, name: String, personal: bool) -> Result<Value, String> {
        service::blocking(move || {
            let home = home()?;
            let id = addin_build::id_from_name(&name);
            let parent = if personal {
                addins::personal_dir(&home)
            } else {
                let (_, root) = project(&home, &project_id)?.ok_or("Project add-ins need a project window.")?;
                addins::project_dir(&root)
            };
            let dir = addin_build::scaffold(&parent, &id, &name)?;
            changed();
            let scope = if personal { Scope::Personal } else { Scope::Project { project_id } };
            Ok(json!({"key": format!("{}/{id}", scope.key()), "dir": dir.display().to_string()}))
        })
        .await
    }
    /// Shows the add-in's folder in the Finder.
    pub async fn reveal(project_id: String, key: String) -> Result<(), String> {
        service::blocking(move || {
            let found = find(&home()?, &project_id, &key)?;
            std::process::Command::new("/usr/bin/open").arg(&found.dir).status().map_err(|error| error.to_string())?;
            Ok(())
        })
        .await
    }
    pub fn subscribe(listener: Callback<Value>, cx: CallCx<'_>) -> Subscription {
        Subscription::start(cx, async move {
            let subscription = changes().subscribe();
            while let Ok(snapshot) = subscription.recv().await {
                listener.post(subscription.latest(snapshot).data);
                next_frame().await;
            }
        })
    }
}
