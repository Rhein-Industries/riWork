//! Add-ins in the app (riwork_core::addins): the ones a window's project can use, trusting
//! them, their settings, new ones from the template, their builds, and the add-in API their
//! engines call (`addin.*` host calls, served here). The engine itself is the `riwork.addin`
//! native view (addin_view.rs).
//!
//! The API answers only what the add-in's manifest was granted; everything that changes what
//! the person sees (a shell tab, a file, another view, a notification, a status item) goes to
//! the add-in's tab in Somtus as an action, and Somtus does it there.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak, atomic::{AtomicBool, Ordering}},
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
        .find(|found| found.key() == key || key.strip_prefix("id:").is_some_and(|id| found.manifest.as_ref().is_ok_and(|manifest| manifest.id == id)))
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
    /// To the add-in's tab in Somtus: actions the person sees.
    pub emit: Box<dyn Fn(&str, Value) + Send + Sync>,
    /// To the add-in's engine: events.
    pub channel: HostChannel,
    workspace: Mutex<Value>,
    closed: AtomicBool,
    host_watch: Mutex<(usize, Option<tokio::task::JoinHandle<()>>)>,
    workers: Mutex<HashMap<String, Arc<WorkerProcess>>>,
}

fn live() -> &'static Mutex<Vec<Weak<Api>>> {
    static LIVE: OnceLock<Mutex<Vec<Weak<Api>>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}

const CALLS: [(&str, Option<Permission>); 20] = [
    ("addin.hosts.get", Some(Permission::HostsRead)),
    ("addin.hosts.subscribe", Some(Permission::HostsRead)),
    ("addin.hosts.unsubscribe", Some(Permission::HostsRead)),
    ("addin.workspace.get", Some(Permission::WorkspaceRead)),
    ("addin.workspace.command", Some(Permission::WorkspaceControl)),
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

/// Never forward arbitrary window payloads, session records, paths or drafts.
/// Limits also keep a misconfigured host snapshot from overwhelming an add-in.
fn workspace_context(value: &Value, project_id: &str) -> Value {
    fn text(value: &Value, limit: usize) -> Option<String> {
        value.as_str().map(|text| text.chars().take(limit).collect())
    }
    fn pane_id(value: &Value) -> Option<u64> {
        value.as_u64().filter(|id| *id > 0 && *id <= u32::MAX as u64)
    }
    let panes = value["panes"].as_array().into_iter().flatten().take(64).filter_map(|pane| {
        let id = pane_id(&pane["id"])?;
        let tabs = pane["tabs"].as_array().into_iter().flatten().take(256).filter_map(|tab| {
            let id = text(&tab["id"], 256).filter(|id| !id.is_empty())?;
            Some(json!({"id": id, "title": text(&tab["title"], 512).unwrap_or_default(),
                "kind": text(&tab["kind"], 64).unwrap_or_default(), "status": text(&tab["status"], 64)}))
        }).collect::<Vec<_>>();
        Some(json!({"id": id, "active": pane["active"].as_bool().unwrap_or(false),
            "activeTab": text(&pane["activeTab"], 256), "tabs": tabs}))
    }).collect::<Vec<_>>();
    let mut result = json!({
        "projectId": (!project_id.is_empty()).then_some(project_id),
        "activePane": pane_id(&value["activePane"]), "panes": panes,
        "selectedWorktreeId": text(&value["selectedWorktreeId"], 256),
        "selectedTaskId": text(&value["selectedTaskId"], 256),
    });
    if let Some(id) = text(&value["windowId"], 256) { result["windowId"] = json!(id); }
    result
}

const WORKSPACE_COMMANDS: &[&str] = &[
    "openProjects", "openFiles", "openPreview", "openSessions", "openSchedules", "openSettings", "openProjectSettings",
    "panel:worktrees", "panel:tasks", "panel:shells", "panel:usage", "panel:addins", "openCustomization",
    "newTab", "newShell", "newChat", "newSubshell", "openVim", "appPreview",
    "openCodex", "openClaude", "openGrok", "openMuse",
    "splitRight", "splitDown", "closeTab", "closePane", "gatherTabs", "defaultLayout",
    "toggleFocusMode", "focusCentered", "nextTab", "previousTab",
    "selectTab1", "selectTab2", "selectTab3", "selectTab4", "selectTab5", "selectTab6", "selectTab7", "selectTab8", "selectTab9",
    "newProjectWindow", "openProjectOrchestrator",
];

/// The native boundary accepts only declared commands and their exact argument shape.
/// The owning workspace validates tab/pane/project membership when it dispatches.
fn workspace_command(value: &Value) -> Result<Value, String> {
    let request = value.as_object().ok_or("A workspace command must be an object.")?;
    if request.keys().any(|key| key != "id" && key != "args") { return Err("Unknown workspace command field.".into()); }
    let id = value["id"].as_str().ok_or("A workspace command needs an id.")?;
    let args = match value.get("args") {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(args)) => args.clone(),
        _ => return Err("Workspace command args must be an object.".into()),
    };
    if matches!(id, "selectTab" | "selectPane" | "switchProject") {
        if args.len() != 1 || !args.contains_key("id") { return Err("This workspace command needs only args.id.".into()); }
        let valid = if id == "selectPane" {
            args["id"].as_u64().is_some_and(|id| id > 0 && id <= u32::MAX as u64)
        } else {
            args["id"].as_str().is_some_and(|id| !id.trim().is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        };
        if !valid { return Err("Invalid workspace selection id.".into()); }
    } else if !WORKSPACE_COMMANDS.contains(&id) {
        return Err(format!("Unsupported workspace command {id:?}."));
    } else if !args.is_empty() {
        return Err("This workspace command takes no args.".into());
    }
    Ok(json!({"type": "command", "id": id, "args": args}))
}

impl Api {
    pub fn new(
        home: PathBuf,
        found: &Found,
        project_id: String,
        channel: HostChannel,
        emit: Box<dyn Fn(&str, Value) + Send + Sync>,
        workspace: Value,
    ) -> Result<Arc<Self>, String> {
        let manifest = manifest(found)?.clone();
        let api = Arc::new(Self {
            home, key: found.key(), scope: found.scope.clone(), dir: found.dir.clone(), manifest,
            workspace: Mutex::new(workspace_context(&workspace, &project_id)),
            project_id, emit, channel, closed: AtomicBool::new(false), host_watch: Mutex::new((0,None)), workers: Mutex::default(),
        });
        let mut live = live().lock().unwrap();
        live.retain(|api| api.strong_count() > 0);
        live.push(Arc::downgrade(&api));
        Ok(api)
    }

    /// A prop update changes only metadata. It never rebuilds the add-in engine.
    pub fn set_workspace(&self, value: &Value) {
        if self.closed.load(Ordering::Acquire) { return; }
        let value = workspace_context(value, &self.project_id);
        let mut previous = self.workspace.lock().unwrap();
        if *previous == value { return; }
        *previous = value.clone();
        drop(previous);
        if self.manifest.allows(Permission::WorkspaceRead) {
            let _ = self.channel.send("addin.workspace", value);
        }
    }

    /// Palette changes are visual metadata available to every running add-in.
    pub fn palette_changed(&self, value: &Value) {
        if self.closed.load(Ordering::Acquire) { return; }
        let palette = value.as_object().into_iter().flatten()
            .filter_map(|(key, value)| value.as_str().map(|color| (key.clone(), json!(color))))
            .collect::<serde_json::Map<_, _>>();
        let _ = self.channel.send("addin.palette", Value::Object(palette));
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
        if self.closed.load(Ordering::Acquire) { return Err("The add-in's view has closed or its trust was revoked.".into()); }
        // Keep enforcement at the actual API dispatch too, including direct callers.
        if let Some((_, Some(permission))) = CALLS.iter().find(|(call, _)| *call == name) {
            if !self.manifest.allows(*permission) {
                return Err(format!("Missing add-in permission {}.", serde_json::to_value(permission).unwrap()));
            }
        }
        let text = |field: &str| args.get(field).and_then(Value::as_str).unwrap_or_default().to_owned();
        match name {
            "addin.hosts.get" => {
                let home = self.home.clone();
                service::blocking(move || Ok(crate::hosts::observe(&home)?.snapshot())).await
            }
            "addin.hosts.subscribe" => {
                let home = self.home.clone();
                let hosts = service::blocking(move || crate::hosts::observe(&home)).await?;
                let mut watch = self.host_watch.lock().unwrap();
                if self.closed.load(Ordering::Acquire) { return Err("The add-in view has closed".into()); }
                if watch.0 >= 64 { return Err("Too many host subscriptions".into()); }
                watch.0 += 1;
                if watch.1.is_none() {
                    let subscription = hosts.subscribe();
                    let weak = Arc::downgrade(&self);
                    watch.1 = Some(runtime().spawn(async move {
                        while let Ok(snapshot) = subscription.recv().await {
                            let Some(api) = weak.upgrade() else { break };
                            if api.closed.load(Ordering::Acquire) { break; }
                            let event = subscription.latest(snapshot);
                            let _ = api.channel.send("addin.hosts", serde_json::to_value(event).unwrap());
                        }
                    }));
                }
                Ok(hosts.snapshot())
            }
            "addin.hosts.unsubscribe" => {
                let mut watch = self.host_watch.lock().unwrap();
                watch.0 = watch.0.saturating_sub(1);
                if watch.0 == 0 { if let Some(task) = watch.1.take() { task.abort(); } }
                Ok(Value::Null)
            }
            "addin.workspace.get" => Ok(self.workspace.lock().unwrap().clone()),
            "addin.workspace.command" => {
                let action = workspace_command(&args)?;
                (self.emit)("action", action);
                Ok(Value::Null)
            }
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
        self.closed.store(true, Ordering::Release);
        let mut watch = self.host_watch.lock().unwrap();
        watch.0 = 0;
        if let Some(task) = watch.1.take() { task.abort(); }
        drop(watch);
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
            addins::trust(&home, &found.key(), &digest)?;
            changed();
            list(&project_id)
        })
        .await
    }
    pub async fn revoke(project_id: String, key: String) -> Result<Value, String> {
        service::blocking(move || {
            let home = home()?;
            // A canonical grant can still be revoked after its source disappears.
            let key = if key.starts_with("id:") { find(&home, &project_id, &key)?.key() } else { key };
            addins::revoke(&home, &key)?;
            for api in live().lock().unwrap().iter().filter_map(Weak::upgrade) {
                if api.key == key && api.home == home {
                    api.close();
                    (api.emit)("state", json!({"status":"untrusted","key":key}));
                }
            }
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
            notify_settings(&found.key(), &home);
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

#[cfg(test)]
mod tests {
    use super::*;
    use nsh_core::{ApproxTextEngine, MountBatch};
    use nsh_engine::{Bundle, Engine, EngineConfig, EngineControl, MountSink};
    use std::sync::mpsc;

    fn manifest(permissions: Value) -> Manifest {
        Manifest::parse(&json!({"id":"example.navigator","name":"Navigator","api":1,
            "views":[{"id":"main","title":"Workspace"}],"permissions":permissions}).to_string()).unwrap()
    }

    fn api(permissions: Value) -> (Arc<Api>, Arc<Mutex<Vec<Value>>>) {
        let found = Found { scope: Scope::Personal, dir: PathBuf::from("/tmp/somtus-addin-unit"), manifest: Ok(manifest(permissions)) };
        let actions = Arc::new(Mutex::new(Vec::new()));
        let output = actions.clone();
        let api = Api::new(PathBuf::from("/tmp/somtus-addin-unit"), &found, "own-project".into(), HostChannel::new(),
            Box::new(move |_, action| output.lock().unwrap().push(action)), Value::Null).unwrap();
        (api, actions)
    }

    #[test]
    fn workspace_context_is_a_bounded_projection_of_the_owning_window() {
        let input = json!({"projectId":"spoofed","windowId":"w1","activePane":2,
            "selectedWorktreeId":"tree","selectedTaskId":"task","draft":"private","root":"/secret",
            "panes":[{"id":2,"active":true,"activeTab":"shell-1","secret":"hidden","tabs":[
                {"id":"shell-1","title":"Shell","kind":"shell","status":"waiting","output":"secret output"}
            ]}, {"id":-1,"tabs":[]}]});
        assert_eq!(workspace_context(&input, "owner"), json!({"projectId":"owner","windowId":"w1","activePane":2,
            "selectedWorktreeId":"tree","selectedTaskId":"task","panes":[{"id":2,"active":true,"activeTab":"shell-1",
                "tabs":[{"id":"shell-1","title":"Shell","kind":"shell","status":"waiting"}]}]}));
        let long = json!({"panes":[{"id":1,"tabs":[{"id":"x","title":"é".repeat(900)}]}]});
        assert_eq!(workspace_context(&long, "")["panes"][0]["tabs"][0]["title"].as_str().unwrap().chars().count(), 512);
        assert!(workspace_context(&Value::Null, "")["projectId"].is_null());
    }

    #[test]
    fn workspace_commands_reject_unknown_actions_and_argument_smuggling() {
        for id in WORKSPACE_COMMANDS {
            assert_eq!(workspace_command(&json!({"id":id})).unwrap(), json!({"type":"command","id":id,"args":{}}));
        }
        assert!(workspace_command(&json!({"id":"selectTab","args":{"id":"panel:files"}})).is_ok());
        assert!(workspace_command(&json!({"id":"selectPane","args":{"id":2}})).is_ok());
        assert!(workspace_command(&json!({"id":"switchProject","args":{"id":"remote:host:project"}})).is_ok());
        for request in [
            json!({"id":"window.close"}), json!({"id":"quit"}), json!({"id":"restart"}), json!({"id":"shell.run"}),
            json!({"id":"openFiles","args":{"path":"/secret"}}),
            json!({"id":"selectTab","args":{"id":"x","windowId":"another"}}),
            json!({"id":"selectPane","args":{"id":"2"}}), json!({"id":"selectPane","args":{"id":1.5}}),
            json!({"id":"selectTab","args":{"id":"\n"}}), json!({"id":"switchProject","args":{}}),
            json!({"id":"openFiles","args":[]}), json!({"id":"openFiles","host":"system.openWith"}),
        ] { assert!(workspace_command(&request).is_err(), "accepted {request}"); }
    }

    #[tokio::test]
    async fn workspace_read_and_control_are_separately_enforced_at_dispatch() {
        let (none, denied_actions) = api(json!([]));
        assert!(none.clone().call("addin.workspace.get", json!({})).await.unwrap_err().contains("workspace.read"));
        assert!(none.clone().call("addin.workspace.command", json!({"id":"newTab"})).await.unwrap_err().contains("workspace.control"));
        assert!(denied_actions.lock().unwrap().is_empty());
        let (read, read_actions) = api(json!(["workspace.read"]));
        read.set_workspace(&json!({"activePane":3,"panes":[{"id":3,"tabs":[]}],"secret":"never exposed"}));
        let snapshot = read.clone().call("addin.workspace.get", json!({})).await.unwrap();
        assert_eq!(snapshot["activePane"], 3);
        assert!(snapshot.get("secret").is_none());
        assert!(read.clone().call("addin.workspace.command", json!({"id":"openFiles"})).await.is_err());
        assert!(read_actions.lock().unwrap().is_empty());
        let (control, actions) = api(json!(["workspace.control"]));
        assert!(control.clone().call("addin.workspace.get", json!({})).await.is_err());
        control.clone().call("addin.workspace.command", json!({"id":"openFiles"})).await.unwrap();
        assert!(control.clone().call("addin.workspace.command", json!({"id":"restart"})).await.is_err());
        assert_eq!(*actions.lock().unwrap(), [json!({"type":"command","id":"openFiles","args":{}})]);
        control.close();
        assert!(control.clone().call("addin.workspace.command", json!({"id":"newTab"})).await.unwrap_err().contains("revoked"));
        assert_eq!(actions.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn all_host_metadata_requires_its_own_grant_and_closing_aborts_watches() {
        let (workspace,_) = api(json!(["workspace.read","workspace.control"]));
        for method in ["addin.hosts.get","addin.hosts.subscribe","addin.hosts.unsubscribe"] {
            assert!(workspace.clone().call(method,json!({})).await.unwrap_err().contains("hosts.read"));
        }
        let (hosts,_) = api(json!(["hosts.read"]));
        assert!(hosts.manifest.allows(Permission::HostsRead));
        let task = runtime().spawn(std::future::pending::<()>());
        let abort = task.abort_handle();
        *hosts.host_watch.lock().unwrap() = (1,Some(task));
        hosts.close();
        assert_eq!(hosts.host_watch.lock().unwrap().0,0);
        assert!(hosts.host_watch.lock().unwrap().1.is_none());
        let _ = abort;
        assert!(hosts.clone().call("addin.hosts.get",json!({})).await.unwrap_err().contains("revoked"));
    }

    struct Sink { control: Arc<OnceLock<EngineControl>>, events: mpsc::Sender<Value> }
    impl MountSink for Sink {
        fn submit(&self, _: MountBatch) {}
        fn report_error(&self, message: &str) { let _ = self.events.send(json!({"error":message})); }
        fn request_frame(&self) { if let Some(control) = self.control.get() { control.frame(); } }
    }

    #[test]
    fn workspace_updates_reach_the_existing_engine_only_when_read_is_granted() {
        for allowed in [false, true] {
            let (api, _) = api(if allowed { json!(["workspace.read"]) } else { json!([]) });
            api.serve();
            let (tx, rx) = mpsc::channel();
            let events = tx.clone();
            api.channel.handle("test.result", move |mut call| { let _ = events.send(call.take_data()); });
            let bundle = Bundle::source(r#"
                import { host } from 'nsh';
                let count = 0, latest = null, palette = null;
                host.on('addin.workspace', value => { count++; latest = value; });
                host.on('addin.palette', value => { palette = value; });
                host.on('test.probe', () => host.post('test.result', { count, latest, palette }));
                host.on('test.get', async () => {
                    try { host.post('test.result', { context: await host.call('addin.workspace.get') }); }
                    catch (error) { host.post('test.result', { denied: String(error) }); }
                });
                host.post('test.result', { ready: true });
            "#.into()).with_capabilities_json(Some(r#"{"host":["addin.*","test.*"]}"#)).unwrap();
            let mut config = EngineConfig::new(bundle);
            config.ui_sets = vec![nsh_base::registry()];
            config.host = api.channel.clone();
            let control = Arc::new(OnceLock::new());
            let setup = control.clone();
            let engine = Engine::start_with(config, || ApproxTextEngine, Sink { control, events: tx }, move |control| { let _ = setup.set(control.clone()); }).unwrap();
            assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), json!({"ready":true}));
            let context = json!({"activePane":7,"panes":[{"id":7,"tabs":[]}]});
            api.set_workspace(&context);
            api.set_workspace(&context);
            api.palette_changed(&json!({"text":"#ffffff","bg":"#111111","private":{"value":"omitted"}}));
            api.channel.send("test.probe", Value::Null).unwrap();
            let observed = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(observed["count"], if allowed { 1 } else { 0 }, "{observed}");
            assert_eq!(observed["palette"], json!({"text":"#ffffff","bg":"#111111"}));
            if allowed { assert_eq!(observed["latest"]["activePane"], 7); }
            else { assert!(observed["latest"].is_null()); }
            api.channel.send("test.get", Value::Null).unwrap();
            let answer = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            if allowed { assert_eq!(answer["context"]["activePane"], 7); }
            else { assert!(answer["denied"].as_str().unwrap().contains("workspace.read")); }
            api.close();
            api.set_workspace(&json!({"activePane":8,"panes":[{"id":8,"tabs":[]}]}));
            api.palette_changed(&json!({"text":"#000000"}));
            api.channel.send("test.probe", Value::Null).unwrap();
            let closed = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(closed["count"], observed["count"], "closing or revoking stops context events");
            assert_eq!(closed["palette"], observed["palette"], "closing or revoking stops palette events");
            drop(engine);
        }
    }

    #[test]
    fn portable_id_resolves_to_the_canonical_trust_key() {
        let home = tempfile::tempdir().unwrap();
        let dir = addins::personal_dir(home.path()).join("navigator");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("addin.json"), serde_json::to_vec(&manifest(json!(["workspace.read"]))).unwrap()).unwrap();
        let found = find(home.path(), "", "id:example.navigator").unwrap();
        assert_eq!(found.key(), "personal/example.navigator");
        let digest = grant(&found).unwrap();
        addins::trust(home.path(), &found.key(), &digest).unwrap();
        assert!(is_trusted(home.path(), &find(home.path(), "", "id:example.navigator").unwrap()));
        assert!(find(home.path(), "", "id:missing").is_err());
    }
}
