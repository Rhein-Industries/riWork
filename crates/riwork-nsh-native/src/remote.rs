//! Paired-Mac presentation bridge. All transport/crypto stays in the existing
//! remote CLI/daemon; RemoteTree owns visibility, retry cadence and creation policy.
//! Tests inject a backend: fixture builds never discover a CLI or open a socket.
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, PoisonError, Weak},
    thread,
    time::{Duration, Instant},
};

use crate::{
    Subscription,
    hub::{Hub, Snapshot, TopicSubscription},
    service,
};
use nsh_core::{
    JsonValue,
    element::{JsHost, JsIn, JsOwned, PropError, TsTypes},
    prelude::*,
};
use riwork_engine::{
    project_sort::ProjectOrder,
    remote_cache::{self, Kept},
    remote_hosts::{Daemons, PairRequest, RemoteCli, RemoteError},
    remote_tree::{
        self as tree, Link, NewShellKind, RemoteTree, Reply, Request, SelectedWants, ShellScope,
        Wants,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWants {
    pub folders: bool,
    pub settings: bool,
    pub collapsed: Vec<String>,
    pub tab_hosts: Vec<String>,
    pub project_key: Option<String>,
    pub worktrees: bool,
    pub tasks: bool,
    pub shells: bool,
}
impl RemoteWants {
    fn engine(&self) -> Result<Wants, String> {
        let selected = self
            .project_key
            .as_deref()
            .map(|key| {
                let (host, project) = remote_parts(key)?;
                Ok::<_, String>(SelectedWants {
                    host: host.into(),
                    project: project.into(),
                    worktrees: self.worktrees,
                    tasks: self.tasks,
                    shells: self.shells,
                })
            })
            .transpose()?;
        Ok(Wants {
            folders: self.folders,
            settings: self.settings,
            collapsed: self.collapsed.iter().cloned().collect(),
            tab_hosts: self
                .tab_hosts
                .iter()
                .map(|id| identifier(id))
                .collect::<Result<_, _>>()?,
            selected,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteHostDto {
    pub id: String,
    pub label: String,
    pub link: Option<String>,
    pub status: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProjectDto {
    pub key: String,
    pub id: String,
    pub name: String,
    pub dimmed: bool,
    pub stats: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFolderDto {
    pub host: String,
    pub label: String,
    pub link: Option<String>,
    pub count: u32,
    pub creating: bool,
    pub failure: Option<String>,
    pub note: Option<String>,
    pub projects: Vec<RemoteProjectDto>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorktreeDto {
    pub id: String,
    pub branch: String,
    pub path: String,
    pub primary: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskDto {
    pub id: String,
    pub title: String,
    pub details: String,
    pub status: String,
    pub worktree_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteShellDto {
    pub id: String,
    pub project_id: Option<String>,
    pub worktree_id: Option<String>,
    pub orchestrator: bool,
    pub chat: bool,
    pub harness: Option<String>,
    pub alive: bool,
    pub cwd: String,
    pub display: String,
}
impl From<tree::RemoteShell> for RemoteShellDto {
    fn from(shell: tree::RemoteShell) -> Self {
        let display = shell.display();
        Self {
            id: shell.id,
            project_id: shell.project_id,
            worktree_id: shell.worktree_id,
            orchestrator: shell.orchestrator,
            chat: shell.chat,
            harness: shell.harness,
            alive: shell.alive,
            cwd: shell.cwd,
            display,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProjectLists {
    pub key: String,
    pub host: String,
    pub project: String,
    pub host_label: String,
    pub project_name: String,
    pub link: Option<String>,
    pub unpaired: bool,
    pub worktrees: Vec<RemoteWorktreeDto>,
    pub worktrees_loading: bool,
    pub worktrees_error: Option<String>,
    pub tasks: Vec<RemoteTaskDto>,
    pub tasks_loading: bool,
    pub tasks_error: Option<String>,
    pub shells: Vec<RemoteShellDto>,
    pub shells_loading: bool,
    pub shells_error: Option<String>,
    pub creating: bool,
    pub failure: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSnapshot {
    pub schema: u32,
    pub hosts: Vec<RemoteHostDto>,
    pub hosts_error: Option<String>,
    pub folders: Vec<RemoteFolderDto>,
    pub projects: Vec<RemoteProjectLists>,
    pub selected: Option<String>,
}
impl JsValue for Snapshot<RemoteSnapshot> {
    fn ts_type(types: &mut TsTypes) -> String {
        let data = RemoteSnapshot::ts_type(types);
        types.declare("RemoteEvent", |_| format!("export interface RemoteEvent {{ kind: 'snapshot'; rev: number; data: {data}; resync?: boolean; }}"))
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        serde_json::from_value(JsonValue::from_js(value)?)
            .map_err(|error| PropError(error.to_string()))
    }
    fn to_js<'a>(&self, host: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        serde_json::to_value(self)
            .map_err(|error| PropError(error.to_string()))?
            .to_js(host)
    }
}
#[derive(Clone, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct PairDesktopRequest {
    pub name: String,
    pub relay: String,
    pub routes: String,
}
// Secret is returned only to the requesting dialog. Never put this in a topic,
// cache, Debug implementation, notice, analytics or fixture inspection payload.
#[derive(Clone, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemotePairingLink {
    pub link: String,
    pub masked: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDefaults {
    pub name: String,
    pub relay: String,
    pub routes: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCreateShell {
    pub project_key: String,
    pub worktree_id: Option<String>,
    pub kind: String,
    pub unrestricted: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorktreeAction {
    pub action: String,
    pub shell: Option<RemoteShellDto>,
    pub shell_id: Option<String>,
}

/// The only side-effect seam. Fake implementations must not delegate to EngineBackend.
pub trait RemoteBackend: Send + Sync {
    fn run(&self, request: &Request) -> Reply;
    fn add_host(&self, link: &str, label: &str) -> Result<(), String>;
    fn remove_host(&self, host: &str) -> Result<(), String>;
    fn pair_desktop(&self, request: &PairRequest) -> Result<String, String>;
    fn create_project(&self, host: &str, name: &str) -> Result<tree::RemoteProject, RemoteError>;
    fn create_shell(
        &self,
        host: &str,
        scope: &ShellScope,
        kind: NewShellKind,
        unrestricted: bool,
    ) -> Result<tree::RemoteShell, RemoteError>;
}
struct Transport {
    cli: RemoteCli,
    daemons: Daemons,
}
pub struct EngineBackend {
    transport: Mutex<Option<Arc<Transport>>>,
}
impl EngineBackend {
    pub fn new() -> Self {
        Self {
            transport: Mutex::new(None),
        }
    }
    fn transport(&self) -> Result<Arc<Transport>, String> {
        let mut cached = self
            .transport
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(transport) = &*cached {
            return Ok(transport.clone());
        }
        // Failed discovery is not cached: an installed companion is found next time.
        let cli = RemoteCli::locate()?;
        let transport = Arc::new(Transport {
            daemons: Daemons::new(cli.clone()),
            cli,
        });
        *cached = Some(transport.clone());
        Ok(transport)
    }
}
impl Default for EngineBackend {
    fn default() -> Self {
        Self::new()
    }
}
impl RemoteBackend for EngineBackend {
    fn run(&self, request: &Request) -> Reply {
        let transport = match self.transport() {
            Ok(value) => value,
            Err(error) => return failed(request, error),
        };
        let list = |host: &str, method: &str, params| {
            transport
                .daemons
                .call(host, method, params, tree::LIST_TIMEOUT)
                .map_err(|e| e.message())
        };
        match request {
            Request::Hosts => Reply::Hosts(transport.cli.hosts()),
            Request::Status { host } => {
                Reply::Status(transport.daemons.status(host).map_err(|e| e.message()))
            }
            Request::Projects { host } => Reply::Projects(
                list(host, "projects.list", json!({})).and_then(|v| tree::parse_projects(&v)),
            ),
            Request::Orchestrators { host } => Reply::Orchestrators(
                list(host, "orchestrators.list", json!({}))
                    .and_then(|v| tree::parse_orchestrators(&v)),
            ),
            Request::Worktrees { host, project } => Reply::Worktrees(
                list(host, "worktrees.list", json!({"project_id": project}))
                    .and_then(|v| tree::parse_worktrees(&v)),
            ),
            Request::Tasks { host, project } => Reply::Tasks(
                list(host, "tasks.list", json!({"project_id": project}))
                    .and_then(|v| tree::parse_tasks(&v)),
            ),
            Request::Shells { host, project } => Reply::Shells(
                list(host, "shells.list", json!({"project_id": project}))
                    .and_then(|v| tree::parse_shells(&v)),
            ),
        }
    }
    fn add_host(&self, link: &str, label: &str) -> Result<(), String> {
        self.transport()?.cli.add_host(link, label)
    }
    fn remove_host(&self, host: &str) -> Result<(), String> {
        let transport = self.transport()?;
        transport.cli.remove_host(host)?;
        transport.daemons.forget(host);
        Ok(())
    }
    fn pair_desktop(&self, request: &PairRequest) -> Result<String, String> {
        self.transport()?
            .cli
            .pair_desktop(request)
            .map(|link| link.expose().to_owned())
    }
    fn create_project(&self, host: &str, name: &str) -> Result<tree::RemoteProject, RemoteError> {
        let transport = self.transport().map_err(RemoteError::Unreachable)?;
        let reply = transport.daemons.call(
            host,
            "project.create",
            tree::project_create_params(name),
            tree::CREATE_TIMEOUT,
        )?;
        tree::parse_created_project(&reply).map_err(RemoteError::Protocol)
    }
    fn create_shell(
        &self,
        host: &str,
        scope: &ShellScope,
        kind: NewShellKind,
        unrestricted: bool,
    ) -> Result<tree::RemoteShell, RemoteError> {
        let transport = self.transport().map_err(RemoteError::Unreachable)?;
        let reply = transport.daemons.call(
            host,
            "shell.create",
            tree::shell_create_params(scope, kind, unrestricted),
            tree::CREATE_TIMEOUT,
        )?;
        tree::parse_created_shell(&reply).map_err(RemoteError::Protocol)
    }
}
fn failed(request: &Request, error: String) -> Reply {
    match request {
        Request::Hosts => Reply::Hosts(Err(error)),
        Request::Status { .. } => Reply::Status(Err(error)),
        Request::Projects { .. } => Reply::Projects(Err(error)),
        Request::Orchestrators { .. } => Reply::Orchestrators(Err(error)),
        Request::Worktrees { .. } => Reply::Worktrees(Err(error)),
        Request::Tasks { .. } => Reply::Tasks(Err(error)),
        Request::Shells { .. } => Reply::Shells(Err(error)),
    }
}

#[cfg(feature = "test-support")]
struct ClosedFixtureBackend;
#[cfg(feature = "test-support")]
impl RemoteBackend for ClosedFixtureBackend {
    fn run(&self, request: &Request) -> Reply {
        if matches!(request, Request::Hosts) {
            Reply::Hosts(Ok(Vec::new()))
        } else {
            failed(
                request,
                "Remote transport is disabled in fixture builds.".into(),
            )
        }
    }
    fn add_host(&self, _: &str, _: &str) -> Result<(), String> {
        Err("unavailable: inject a disposable remote fixture backend".into())
    }
    fn remove_host(&self, _: &str) -> Result<(), String> {
        self.add_host("", "")
    }
    fn pair_desktop(&self, _: &PairRequest) -> Result<String, String> {
        self.add_host("", "").map(|_| String::new())
    }
    fn create_project(&self, _: &str, _: &str) -> Result<tree::RemoteProject, RemoteError> {
        Err(RemoteError::Unreachable(
            "Remote fixture has no transport".into(),
        ))
    }
    fn create_shell(
        &self,
        _: &str,
        _: &ShellScope,
        _: NewShellKind,
        _: bool,
    ) -> Result<tree::RemoteShell, RemoteError> {
        Err(RemoteError::Unreachable(
            "Remote fixture has no transport".into(),
        ))
    }
}

struct State {
    tree: RemoteTree,
    selected: Option<String>,
    observers: BTreeMap<u64, RemoteWants>,
    next_observer: u64,
    defaults: RemoteDefaults,
}
pub struct RemoteService {
    home: PathBuf,
    backend: Arc<dyn RemoteBackend>,
    state: Mutex<State>,
    cache_write: Mutex<()>,
    host_mutation: Mutex<()>,
    hub: Hub<RemoteSnapshot>,
}
struct Observation {
    service: Weak<RemoteService>,
    id: u64,
}
impl Drop for Observation {
    fn drop(&mut self) {
        if let Some(service) = self.service.upgrade() {
            service
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .observers
                .remove(&self.id);
        }
    }
}
impl RemoteService {
    /// `home` must be host supplied; JS never chooses a filesystem root.
    /// Tests use a disposable directory and a fake backend, with no CLI or sockets.
    pub fn new(home: PathBuf, backend: Arc<dyn RemoteBackend>) -> Arc<Self> {
        let kept = remote_cache::load(&home);
        let mut tree = RemoteTree::default();
        tree.keep(kept.hosts);
        let service = Arc::new(Self {
            home: home.clone(),
            backend,
            state: Mutex::new(State {
                tree,
                selected: kept.selected,
                observers: BTreeMap::new(),
                next_observer: 0,
                defaults: RemoteDefaults {
                    routes: home
                        .join("remote/relay-routes.json")
                        .to_string_lossy()
                        .into_owned(),
                    ..Default::default()
                },
            }),
            cache_write: Mutex::new(()),
            host_mutation: Mutex::new(()),
            hub: Hub::new(RemoteSnapshot {
                schema: 1,
                ..Default::default()
            }),
        });
        service.publish();
        service
    }
    fn publish(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let dto = snapshot(&state);
        self.hub.publish(dto);
        // Serialize cache snapshots and writes under one dedicated order. Every
        // writer captures current state after taking this gate, so an old reply
        // cannot overwrite a newer selection/list. No JS thread calls publish.
        if state.tree.take_kept_dirty() {
            drop(state);
            self.save_cache();
        }
    }
    fn save_cache(&self) {
        let _write = self
            .cache_write
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let kept = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            Kept {
                selected: state.selected.clone(),
                hosts: state.tree.kept(),
            }
        };
        // It is a cache: a failed write must not turn a sent mutation into a
        // failure the person might repeat. No pairing material is kept here.
        let _ = remote_cache::save(&self.home, &kept);
    }
    pub fn current(&self) -> RemoteSnapshot {
        snapshot(&self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }
    fn observe(
        self: &Arc<Self>,
        wants: RemoteWants,
    ) -> Result<(Observation, TopicSubscription<RemoteSnapshot>), String> {
        wants.engine()?;
        let id = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.next_observer += 1;
            let id = state.next_observer;
            state.observers.insert(id, wants);
            id
        };
        self.publish();
        Ok((
            Observation {
                service: Arc::downgrade(self),
                id,
            },
            self.hub.subscribe(),
        ))
    }
    /// Plans are memory-only; requests run without holding the state lock.
    /// Calling once per observer preserves RemoteTree's cross-window in-flight
    /// de-duplication, including two different selected projects on the same Mac.
    fn plan(&self, now: Instant) -> Vec<Request> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let wants: Vec<_> = state
            .observers
            .values()
            .filter_map(|w| w.engine().ok())
            .collect();
        wants
            .into_iter()
            .flat_map(|wants| state.tree.plan(now, &wants))
            .collect()
    }
    fn poll(self: &Arc<Self>) {
        for request in self.plan(Instant::now()) {
            let service = self.clone();
            thread::spawn(move || {
                let reply = service.backend.run(&request);
                service
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .tree
                    .apply(&request, reply);
                service.publish();
            });
        }
    }
    pub fn select(&self, key: Option<String>) -> Result<(), String> {
        if let Some(key) = &key {
            remote_parts(key)?;
        }
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .selected = key;
        self.save_cache();
        self.publish();
        Ok(())
    }
    pub fn add_host(&self, link: String, label: String) -> Result<(), String> {
        let link = check_link(&link)?;
        let _mutation = self
            .host_mutation
            .try_lock()
            .map_err(|_| "conflict: a remote host change is already running")?;
        self.backend
            .add_host(&link, label.trim())
            .map_err(|e| secret_error(&e, Some(&link)))?;
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tree
            .invalidate_hosts();
        Ok(())
    }
    pub fn remove_host(&self, host: String) -> Result<(), String> {
        let host = identifier(&host)?;
        let _mutation = self
            .host_mutation
            .try_lock()
            .map_err(|_| "conflict: a remote host change is already running")?;
        self.backend
            .remove_host(&host)
            .map_err(|e| secret_error(&e, None))?;
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tree
            .invalidate_hosts();
        Ok(())
    }
    pub fn pair_desktop(&self, request: PairDesktopRequest) -> Result<RemotePairingLink, String> {
        if request.name.trim().is_empty()
            || request.relay.trim().is_empty()
            || request.routes.trim().is_empty()
        {
            return Err("invalid: Fill in the name, the relay and the routes file.".into());
        }
        let _mutation = self
            .host_mutation
            .try_lock()
            .map_err(|_| "conflict: a remote host change is already running")?;
        let pair = PairRequest {
            name: request.name.trim().into(),
            relay: request.relay.trim().into(),
            routes: PathBuf::from(request.routes.trim()),
        };
        let link = self
            .backend
            .pair_desktop(&pair)
            .map_err(|e| secret_error(&e, None))?;
        let link = check_link(&link)?;
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .defaults = RemoteDefaults {
            name: pair.name,
            relay: pair.relay,
            routes: pair.routes.to_string_lossy().into_owned(),
        };
        let masked = masked_link(&link);
        Ok(RemotePairingLink { link, masked })
    }
    pub fn create_project(&self, host: String, name: String) -> Result<RemoteProjectDto, String> {
        let host = identifier(&host)?;
        let name = tree::validate_project_name(&name).map_err(|e| format!("invalid: {e}"))?;
        if !self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tree
            .begin_project(&host)
        {
            return Err(
                "conflict: this Mac is unpaired or a project is already being created".into(),
            );
        }
        self.publish();
        let result = self.backend.create_project(&host, &name);
        let uncertain = result.as_ref().err().is_some_and(tree::outcome_unknown);
        let (project, failure) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let project = state.tree.finish_project(&host, result);
            let failure = state
                .tree
                .folders("", ProjectOrder::default(), &HashSet::new(), None)
                .into_iter()
                .find(|folder| folder.host == host)
                .and_then(|folder| folder.failure);
            (project, failure)
        };
        self.publish();
        project.map(|p| RemoteProjectDto { key: tree::project_key(&host, &p.id), id: p.id, name: p.name, dimmed: false, stats: tree::Stats::default().line() })
            .ok_or_else(|| format!("{}: {}", if uncertain || failure.is_none() { "broken" } else { "refused" }, failure.unwrap_or_else(|| "The host was removed while its reply arrived. Check its projects before trying again.".into())))
    }
    pub fn create_shell(&self, request: RemoteCreateShell) -> Result<RemoteShellDto, String> {
        let (host, project) = remote_parts(&request.project_key)?;
        let kind = match request.kind.as_str() {
            "shell" => NewShellKind::Shell,
            "codex" => NewShellKind::Codex,
            "claude" => NewShellKind::Claude,
            "grok" => NewShellKind::Grok,
            "muse" => NewShellKind::Muse,
            _ => return Err("invalid: remote shell kind".into()),
        };
        let scope = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let worktree = request
                .worktree_id
                .as_deref()
                .filter(|id| state.tree.worktree_branch(host, project, id).is_some());
            let scope = worktree
                .map(|id| ShellScope::Worktree(id.into()))
                .unwrap_or_else(|| ShellScope::Project(project.into()));
            if !state.tree.begin_shell(host, project) {
                return Err(
                    "conflict: this Mac is unpaired or a terminal is already starting".into(),
                );
            }
            scope
        };
        self.publish();
        let result = self
            .backend
            .create_shell(host, &scope, kind, request.unrestricted);
        let uncertain = result.as_ref().err().is_some_and(tree::outcome_unknown);
        let (shell, failure) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let shell = state.tree.finish_shell(host, project, result);
            let failure = state
                .tree
                .selected_view(&request.project_key)
                .and_then(|view| view.failure);
            (shell, failure)
        };
        self.publish();
        shell.map(Into::into).ok_or_else(|| format!("{}: {}", if uncertain || failure.is_none() { "broken" } else { "refused" }, failure.unwrap_or_else(|| "The host was removed while its reply arrived. Check its terminals before trying again.".into())))
    }
}

fn link_name(link: Option<Link>) -> Option<String> {
    link.map(|link| {
        match link {
            Link::Online => "online",
            Link::Connecting => "connecting",
            Link::Offline => "offline",
        }
        .into()
    })
}
fn snapshot(state: &State) -> RemoteSnapshot {
    let folders = state
        .tree
        .folders(
            "",
            ProjectOrder::default(),
            &HashSet::new(),
            state.selected.as_deref(),
        )
        .into_iter()
        .map(|folder| RemoteFolderDto {
            host: folder.host,
            label: folder.label,
            link: link_name(folder.link),
            count: folder.count as u32,
            creating: folder.creating,
            failure: folder.failure,
            note: folder.note,
            projects: folder
                .projects
                .into_iter()
                .map(|p| RemoteProjectDto {
                    id: tree::parse_project_key(&p.key)
                        .map(|(_, id)| id.to_owned())
                        .unwrap_or_default(),
                    key: p.key,
                    name: p.name,
                    dimmed: p.dimmed,
                    stats: p.stats.line(),
                })
                .collect(),
        })
        .collect();
    let mut keys: Vec<_> = state
        .observers
        .values()
        .filter_map(|w| w.project_key.clone())
        .collect();
    if let Some(selected) = &state.selected {
        keys.push(selected.clone());
    }
    keys.sort();
    keys.dedup();
    let projects = keys
        .into_iter()
        .filter_map(|key| {
            state
                .tree
                .selected_view(&key)
                .map(|view| RemoteProjectLists {
                    key,
                    host: view.host,
                    project: view.project,
                    host_label: view.host_label,
                    project_name: view.project_name,
                    link: link_name(view.link),
                    unpaired: view.unpaired,
                    worktrees: view
                        .worktrees
                        .items
                        .into_iter()
                        .map(|w| RemoteWorktreeDto {
                            id: w.id,
                            branch: w.branch,
                            path: w.path,
                            primary: w.primary,
                        })
                        .collect(),
                    worktrees_loading: view.worktrees.loading,
                    worktrees_error: view.worktrees.error,
                    tasks: view
                        .tasks
                        .items
                        .into_iter()
                        .map(|t| RemoteTaskDto {
                            id: t.id,
                            title: t.title,
                            details: t.details,
                            status: t.status.as_str().into(),
                            worktree_id: t.worktree_id,
                        })
                        .collect(),
                    tasks_loading: view.tasks.loading,
                    tasks_error: view.tasks.error,
                    shells: view.shells.items.into_iter().map(Into::into).collect(),
                    shells_loading: view.shells.loading,
                    shells_error: view.shells.error,
                    creating: view.creating,
                    failure: view.failure,
                })
        })
        .collect();
    RemoteSnapshot {
        schema: 1,
        hosts: state
            .tree
            .hosts()
            .map(|(host, link)| RemoteHostDto {
                id: host.id.clone(),
                label: host.label.clone(),
                link: link_name(link),
                status: link.unwrap_or(Link::Connecting).text().into(),
            })
            .collect(),
        hosts_error: state.tree.hosts_error().map(str::to_owned),
        folders,
        projects,
        selected: state.selected.clone(),
    }
}
fn identifier(id: &str) -> Result<String, String> {
    if id.is_empty()
        || id.len() > 256
        || id
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == ':')
    {
        Err("invalid: remote identifier".into())
    } else {
        Ok(id.into())
    }
}
fn remote_parts(key: &str) -> Result<(&str, &str), String> {
    let (host, project) = tree::parse_project_key(key).ok_or("invalid: remote project key")?;
    identifier(host)?;
    identifier(project)?;
    Ok((host, project))
}
fn check_link(text: &str) -> Result<String, String> {
    let link = text.trim();
    if link.is_empty() {
        return Err("invalid: Paste the pairing link from the other Mac.".into());
    }
    if !link.starts_with("riwork://pair?") || link.contains(char::is_whitespace) {
        return Err("invalid: That is not a RiWork pairing link.".into());
    }
    Ok(link.into())
}
fn masked_link(link: &str) -> String {
    format!(
        "{}…  ({} characters)",
        link.chars().take(16).collect::<String>(),
        link.chars().count()
    )
}
fn secret_error(error: &str, secret: Option<&str>) -> String {
    let mut text = error.to_owned();
    if let Some(secret) = secret.filter(|s| !s.is_empty()) {
        text = text.replace(secret, "[pairing link hidden]");
    }
    // Defensive scrub for a fake/backend that violated RemoteCli's error contract.
    text.split_whitespace()
        .map(|part| {
            if part.contains("riwork://pair?") {
                "[pairing link hidden]"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

static PROCESS: OnceLock<Arc<RemoteService>> = OnceLock::new();
pub fn install_service(remote: Arc<RemoteService>) -> Result<(), String> {
    PROCESS
        .set(remote)
        .map_err(|_| "conflict: remote service is already installed".into())
}
fn process() -> Result<Arc<RemoteService>, String> {
    if let Some(remote) = PROCESS.get() {
        return Ok(remote.clone());
    }
    let home = service::process_home()?;
    #[cfg(feature = "test-support")]
    let backend: Arc<dyn RemoteBackend> = Arc::new(ClosedFixtureBackend);
    #[cfg(not(feature = "test-support"))]
    let backend: Arc<dyn RemoteBackend> = Arc::new(EngineBackend::new());
    Ok(PROCESS
        .get_or_init(|| RemoteService::new(home, backend))
        .clone())
}
fn start_polling(remote: &Arc<RemoteService>) {
    // Each subscription owns only a lease; a single weak process worker services
    // their union. Starting it is serialized independently of backend operations.
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let remote = Arc::downgrade(remote);
        thread::spawn(move || {
            loop {
                let Some(service) = remote.upgrade() else {
                    break;
                };
                service.poll();
                drop(service);
                thread::sleep(Duration::from_millis(500));
            }
        });
    });
}

#[module]
pub struct Remote;
#[module]
impl Remote {
    pub async fn snapshot() -> Result<RemoteSnapshot, String> {
        service::blocking(|| Ok(process()?.current())).await
    }
    pub fn subscribe(
        wants: RemoteWants,
        listener: Callback<Snapshot<RemoteSnapshot>>,
        cx: CallCx<'_>,
    ) -> Result<Subscription, String> {
        wants.engine()?;
        Ok(Subscription::start(cx, async move {
            if let Ok((lease, topic)) = service::blocking(move || {
                let remote = process()?;
                start_polling(&remote);
                let observation = remote.observe(wants)?;
                remote.poll();
                Ok(observation)
            })
            .await
            {
                let _lease = lease;
                crate::deliver(topic, listener).await;
            }
        }))
    }
    pub async fn select(key: Option<String>) -> Result<(), String> {
        service::blocking(move || process()?.select(key)).await
    }
    pub async fn defaults() -> Result<RemoteDefaults, String> {
        service::blocking(|| {
            Ok(process()?
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .defaults
                .clone())
        })
        .await
    }
    pub async fn add_host(link: String, label: String) -> Result<(), String> {
        service::blocking(move || {
            let remote = process()?;
            remote.add_host(link, label)?;
            remote.poll();
            Ok(())
        })
        .await
    }
    pub async fn remove_host(host: String) -> Result<(), String> {
        service::blocking(move || {
            let remote = process()?;
            remote.remove_host(host)?;
            remote.poll();
            Ok(())
        })
        .await
    }
    pub async fn pair_desktop(request: PairDesktopRequest) -> Result<RemotePairingLink, String> {
        service::blocking(move || process()?.pair_desktop(request)).await
    }
    pub async fn create_project(host: String, name: String) -> Result<RemoteProjectDto, String> {
        service::blocking(move || process()?.create_project(host, name)).await
    }
    pub async fn create_shell(request: RemoteCreateShell) -> Result<RemoteShellDto, String> {
        service::blocking(move || process()?.create_shell(request)).await
    }
    pub async fn dismiss_failure(host: String, project: Option<String>) -> Result<(), String> {
        service::blocking(move || {
            let remote = process()?;
            {
                let mut state = remote.state.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(project) = project {
                    state.tree.dismiss_shell_failure(&host, &project);
                } else {
                    state.tree.dismiss_project_failure(&host);
                }
            }
            remote.publish();
            Ok(())
        })
        .await
    }
    pub async fn tab_title(host: String, shell: String) -> Result<String, String> {
        service::blocking(move || {
            Ok(process()?
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .tree
                .tab_title(&host, &shell))
        })
        .await
    }
    pub async fn orchestrators(host: String) -> Result<Vec<RemoteShellDto>, String> {
        service::blocking(move || {
            let host = identifier(&host)?;
            let remote = process()?;
            let request = Request::Orchestrators { host };
            let reply = remote.backend.run(&request);
            let result = match &reply {
                Reply::Orchestrators(Ok(shells)) => {
                    Ok(shells.iter().cloned().map(Into::into).collect())
                }
                Reply::Orchestrators(Err(e)) => Err(e.clone()),
                _ => Err("broken: unexpected orchestrator reply".into()),
            };
            remote
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .tree
                .apply(&request, reply);
            remote.publish();
            result
        })
        .await
    }
    pub async fn click_worktree(
        key: String,
        worktree: String,
        open_tabs: Vec<String>,
    ) -> Result<RemoteWorktreeAction, String> {
        service::blocking(move || {
            let (host, project) = remote_parts(&key)?;
            let remote = process()?;
            let state = remote.state.lock().unwrap_or_else(PoisonError::into_inner);
            let open = open_tabs.iter().map(String::as_str).collect::<Vec<_>>();
            Ok(
                match state.tree.click_worktree(host, project, &worktree, &open) {
                    tree::WorktreeClick::Tab(id) => RemoteWorktreeAction {
                        action: "tab".into(),
                        shell_id: Some(id),
                        shell: None,
                    },
                    tree::WorktreeClick::Live(shell) => RemoteWorktreeAction {
                        action: "open".into(),
                        shell_id: None,
                        shell: Some(shell.into()),
                    },
                    tree::WorktreeClick::Start => RemoteWorktreeAction {
                        action: "start".into(),
                        shell_id: None,
                        shell: None,
                    },
                    tree::WorktreeClick::Unknown => RemoteWorktreeAction {
                        action: "unknown".into(),
                        shell_id: None,
                        shell: None,
                    },
                },
            )
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use riwork_engine::remote_hosts::{Host, HostStatus, LinkState};
    use std::sync::atomic::{AtomicUsize, Ordering};
    const HOST: &str = "fixture-mac";
    const PROJECT: &str = "fixture-project";
    struct Fake {
        calls: Mutex<Vec<String>>,
        creates: AtomicUsize,
    }
    impl Fake {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                creates: AtomicUsize::new(0),
            })
        }
    }
    impl RemoteBackend for Fake {
        fn run(&self, request: &Request) -> Reply {
            self.calls.lock().unwrap().push(format!("{request:?}"));
            match request {
                Request::Hosts => Reply::Hosts(Ok(vec![Host {
                    id: HOST.into(),
                    label: "Disposable Mac".into(),
                }])),
                Request::Status { .. } => Reply::Status(Ok(HostStatus {
                    state: LinkState::Online,
                    rtt_ms: None,
                    since: None,
                    reason: None,
                    label: None,
                })),
                Request::Projects { .. } => Reply::Projects(Ok(vec![tree::RemoteProject {
                    id: PROJECT.into(),
                    name: "Fixture project".into(),
                    created_at: 1,
                }])),
                _ => failed(request, "fixture list not supplied".into()),
            }
        }
        fn add_host(&self, link: &str, _: &str) -> Result<(), String> {
            Err(format!("fixture rejected {link}"))
        }
        fn remove_host(&self, host: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("remove:{host}"));
            Ok(())
        }
        fn pair_desktop(&self, _: &PairRequest) -> Result<String, String> {
            Ok("riwork://pair?DISPOSABLE_FAKE_NO_CREDENTIAL".into())
        }
        fn create_project(&self, _: &str, _: &str) -> Result<tree::RemoteProject, RemoteError> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            Err(RemoteError::Timeout)
        }
        fn create_shell(
            &self,
            _: &str,
            scope: &ShellScope,
            kind: NewShellKind,
            unrestricted: bool,
        ) -> Result<tree::RemoteShell, RemoteError> {
            self.calls.lock().unwrap().push(format!(
                "create-shell:{scope:?}:{}:{unrestricted}",
                kind.wire()
            ));
            self.creates.fetch_add(1, Ordering::SeqCst);
            Err(RemoteError::Timeout)
        }
    }
    fn fixture() -> (tempfile::TempDir, Arc<RemoteService>, Arc<Fake>) {
        let directory = tempfile::tempdir().unwrap();
        let fake = Fake::new();
        let remote = RemoteService::new(directory.path().into(), fake.clone());
        (directory, remote, fake)
    }
    fn answer(remote: &RemoteService, request: Request) {
        let reply = remote.backend.run(&request);
        remote.state.lock().unwrap().tree.apply(&request, reply);
        remote.publish();
    }
    #[test]
    fn an_unobserved_service_never_requests_even_the_host_registry() {
        let (_directory, remote, fake) = fixture();
        assert!(remote.plan(Instant::now()).is_empty());
        assert!(fake.calls.lock().unwrap().is_empty());
        assert!(remote.current().hosts.is_empty());
    }
    #[test]
    fn visible_windows_share_the_tree_and_closed_observers_stop_requests() {
        let (_directory, remote, fake) = fixture();
        let (first, _topic) = remote
            .observe(RemoteWants {
                settings: true,
                ..Default::default()
            })
            .unwrap();
        let (second, _peer) = remote
            .observe(RemoteWants {
                settings: true,
                ..Default::default()
            })
            .unwrap();
        let planned = remote.plan(Instant::now());
        assert_eq!(planned.len(), 1);
        assert!(matches!(planned[0], Request::Hosts));
        answer(&remote, Request::Hosts);
        assert_eq!(remote.plan(Instant::now()).len(), 1); // status, not a duplicate registry call
        drop(first);
        drop(second);
        assert!(
            remote
                .plan(Instant::now() + Duration::from_secs(40))
                .is_empty()
        );
        assert_eq!(fake.calls.lock().unwrap().len(), 1);
    }
    #[test]
    fn pairing_is_ephemeral_masked_and_failed_redemption_never_echoes_the_secret() {
        let (directory, remote, _) = fixture();
        let link = remote
            .pair_desktop(PairDesktopRequest {
                name: "Fixture".into(),
                relay: "wss://invalid.example/v1/ws".into(),
                routes: directory
                    .path()
                    .join("disposable-routes.json")
                    .to_string_lossy()
                    .into_owned(),
            })
            .unwrap();
        // Desktop remote_prompt::masked exposes exactly 16 characters ("DI",
        // not the seventeenth "S") and the total length, never the whole link.
        assert_eq!(
            link.masked,
            format!(
                "riwork://pair?DI…  ({} characters)",
                link.link.chars().count()
            )
        );
        assert!(!link.masked.contains("NO_CREDENTIAL"));
        assert!(
            !remote
                .add_host(link.link.clone(), "Fixture".into())
                .unwrap_err()
                .contains(&link.link)
        );
        remote
            .select(Some(tree::project_key(HOST, PROJECT)))
            .unwrap();
        let saved = std::fs::read_to_string(directory.path().join("remote-ui.json")).unwrap();
        assert!(!saved.contains(&link.link));
        assert!(
            !serde_json::to_string(&remote.current())
                .unwrap()
                .contains(&link.link)
        );
        assert!(!directory.path().join("disposable-routes.json").exists()); // fake never writes pairing material
    }
    #[test]
    fn lost_creation_is_never_resent_and_selection_cache_uses_remote_namespace() {
        let (directory, remote, fake) = fixture();
        answer(&remote, Request::Hosts);
        answer(&remote, Request::Status { host: HOST.into() });
        answer(&remote, Request::Projects { host: HOST.into() });
        let key = tree::project_key(HOST, PROJECT);
        remote.select(Some(key.clone())).unwrap();
        assert!(
            remote
                .create_project(HOST.into(), "New remote".into())
                .is_err()
        );
        for _ in 0..4 {
            let _ = remote.plan(Instant::now());
        }
        assert_eq!(fake.creates.load(Ordering::SeqCst), 1);
        assert!(
            remote.current().folders[0]
                .failure
                .as_ref()
                .unwrap()
                .contains("may exist")
        );
        assert_eq!(
            remote_cache::load(directory.path()).selected.as_deref(),
            Some(key.as_str())
        );
        remote.select(None).unwrap();
        assert_eq!(remote_cache::load(directory.path()).selected, None);
        assert!(remote.select(Some("local-project".into())).is_err());
    }
    #[test]
    fn stale_host_lists_remain_dimmed_and_removed_hosts_are_unpaired() {
        let (_directory, remote, _) = fixture();
        answer(&remote, Request::Hosts);
        answer(&remote, Request::Projects { host: HOST.into() });
        remote
            .select(Some(tree::project_key(HOST, PROJECT)))
            .unwrap();
        let request = Request::Status { host: HOST.into() };
        remote
            .state
            .lock()
            .unwrap()
            .tree
            .apply(&request, Reply::Status(Err("fixture offline".into())));
        let data = remote.current();
        assert_eq!(data.folders[0].projects[0].name, "Fixture project");
        assert!(data.folders[0].projects[0].dimmed);
        remote
            .state
            .lock()
            .unwrap()
            .tree
            .apply(&Request::Hosts, Reply::Hosts(Ok(Vec::new())));
        assert!(remote.current().projects[0].unpaired);
    }
    #[test]
    fn invalid_pairing_and_project_names_never_reach_the_fake() {
        let (_directory, remote, fake) = fixture();
        for link in ["", "https://not-a-pair", "riwork://pair?with space"] {
            assert!(remote.add_host(link.into(), "".into()).is_err());
        }
        assert!(
            remote
                .create_project(HOST.into(), "../escape".into())
                .is_err()
        );
        assert_eq!(fake.creates.load(Ordering::SeqCst), 0);
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn shell_creation_uses_known_worktree_else_project_and_never_retries_a_lost_reply() {
        let (_directory, remote, fake) = fixture();
        answer(&remote, Request::Hosts);
        let key = tree::project_key(HOST, PROJECT);
        remote.select(Some(key.clone())).unwrap();
        answer(&remote, Request::Status { host: HOST.into() });
        let (_observation, _topic) = remote
            .observe(RemoteWants {
                project_key: Some(key.clone()),
                worktrees: true,
                ..Default::default()
            })
            .unwrap();
        // A reply is accepted only after a visible project was planned. Merely
        // remembering the selection does not allocate its remote list state.
        assert!(
            remote
                .plan(Instant::now())
                .iter()
                .any(|request| matches!(request,
            Request::Worktrees { host, project } if host == HOST && project == PROJECT))
        );
        let request = Request::Worktrees {
            host: HOST.into(),
            project: PROJECT.into(),
        };
        assert!(remote.state.lock().unwrap().tree.apply(
            &request,
            Reply::Worktrees(Ok(vec![tree::RemoteWorktree {
                id: "known-tree".into(),
                branch: "main".into(),
                path: "/FAKE/remote".into(),
                primary: true,
            }])),
        ));
        let error = remote
            .create_shell(RemoteCreateShell {
                project_key: key.clone(),
                worktree_id: Some("removed-tree".into()),
                kind: "shell".into(),
                unrestricted: true,
            })
            .unwrap_err();
        assert!(error.starts_with("broken:"));
        assert!(error.contains("shell may exist"));
        assert!(
            fake.calls
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .contains("Project(\"fixture-project\"):shell:true")
        );
        for _ in 0..4 {
            let _ = remote.plan(Instant::now() + Duration::from_secs(100));
        }
        assert_eq!(fake.creates.load(Ordering::SeqCst), 1);
        let _ = remote.create_shell(RemoteCreateShell {
            project_key: key,
            worktree_id: Some("known-tree".into()),
            kind: "codex".into(),
            unrestricted: false,
        });
        assert!(
            fake.calls
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .contains("Worktree(\"known-tree\"):codex:false")
        );
        remote.state.lock().unwrap().tree.begin_shell(HOST, PROJECT);
        let before = fake.creates.load(Ordering::SeqCst);
        assert!(
            remote
                .create_shell(RemoteCreateShell {
                    project_key: tree::project_key(HOST, PROJECT),
                    worktree_id: None,
                    kind: "shell".into(),
                    unrestricted: false
                })
                .unwrap_err()
                .starts_with("conflict:")
        );
        assert_eq!(fake.creates.load(Ordering::SeqCst), before);
    }
}
