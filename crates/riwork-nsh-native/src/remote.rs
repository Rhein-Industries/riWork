//! Paired-Mac presentation bridge. All transport/crypto stays in the existing
//! remote CLI/daemon; RemoteTree owns visibility, retry cadence and creation policy.
//! Tests inject a backend: fixture builds never discover a CLI or open a socket.
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, PoisonError, Weak},
    thread,
    time::{Duration, Instant},
};

use crate::{
    Subscription,
    host_management::{AddSshRequest, SshRoute},
    hub::{Hub, Snapshot, TopicSubscription},
    service,
};
use nsh_core::{
    JsonValue,
    element::{JsHost, JsIn, JsOwned, PropError, TsTypes},
    prelude::*,
};
use riwork_engine::{
    project_sort::{ProjectOrder, ProjectSort},
    remote_cache::{self, Kept},
    remote_hosts::{Daemons, HostStatus, PairRequest, RemoteCli, RemoteError},
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
    /// Every open remote tab (`remote:{host}:{shell}`), shown or not, for its
    /// title. Titles never ask a host for anything.
    #[serde(default)]
    pub tabs: Vec<String>,
    /// The window's Projects order (`name`, `added`, `live`); its remote
    /// folders follow it. Empty means the engine's default.
    #[serde(default)]
    pub order: String,
    #[serde(default)]
    pub descending: bool,
}
impl RemoteWants {
    /// Matches RemoteTree's polling demand after observe() validates the wants.
    /// Cached tab titles alone never request anything from a host.
    fn needs_polling(&self) -> bool {
        self.folders || self.settings || !self.tab_hosts.is_empty() || self.project_key.is_some()
    }
    fn project_order(&self) -> ProjectOrder {
        let by = match self.order.as_str() {
            "name" => ProjectSort::Name,
            "added" => ProjectSort::DateAdded,
            "live" => ProjectSort::LiveSessions,
            "edited" => ProjectSort::LastEdited,
            _ => return ProjectOrder::default(),
        };
        ProjectOrder {
            by,
            descending: self.descending,
        }
    }
    fn engine(&self) -> Result<Wants, String> {
        let selected = self
            .project_key
            .as_deref()
            .map(|key| {
                let (host, project) = remote_parts(key)?;
                // As GPUI's remote_wants: each panel names things by worktree,
                // worktree rows count tasks, and a worktree click decides from
                // the host's shells.
                Ok::<_, String>(SelectedWants {
                    host: host.into(),
                    project: project.into(),
                    worktrees: self.worktrees || self.tasks || self.shells,
                    tasks: self.tasks || self.worktrees,
                    shells: self.shells || self.worktrees,
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
    /// The project keys in each order a visible Projects panel asked for.
    #[serde(default)]
    pub orders: Vec<RemoteFolderOrder>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFolderOrder {
    pub order: String,
    pub descending: bool,
    pub keys: Vec<String>,
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
    pub chat_id: Option<String>,
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
            chat_id: shell.chat_id,
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
    /// Live shells as last listed (what the status bar counts), else none.
    #[serde(default)]
    pub live: Option<u32>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTabTitle {
    pub key: String,
    pub title: String,
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
    /// `⇄ {host} · {detail}` for every open remote tab, from whatever list of
    /// that host knows the shell (any project, its orchestrators, or a creation
    /// reply), else the shell's short id.
    #[serde(default)]
    pub titles: Vec<RemoteTabTitle>,
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
/// The project's orchestrator to open, or the notice that says why there is none.
#[derive(Clone, Debug, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct RemoteOrchestrator {
    pub shell: Option<RemoteShellDto>,
    pub notice: Option<String>,
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
    /// One host-specific RPC. Implementations must never repeat a mutation.
    fn call(&self, _host: &str, _method: &str, _params: serde_json::Value, _timeout: Duration) -> Result<serde_json::Value, RemoteError> {
        Err(RemoteError::Rpc { code: "unsupported".into(), message: "This remote backend does not support chat RPCs".into() })
    }
    fn run(&self, request: &Request) -> Reply;
    /// Whether this host's daemon is being started right now, so that a request for the host would
    /// only wait for that. It runs under the queue's lock, so it must not wait for anything.
    fn starting(&self, _host: &str) -> bool {
        false
    }
    fn ssh_routes(&self) -> Result<Vec<SshRoute>, String> {
        Err("unavailable: SSH route listing is unavailable on this backend".into())
    }
    fn add_ssh(&self, _request: &AddSshRequest) -> Result<(), String> {
        Err("unavailable: SSH registration is unavailable on this backend".into())
    }
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
        let cli = RemoteCli::locate()?.with_home(service::process_home()?);
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
    fn call(&self, host: &str, method: &str, params: serde_json::Value, timeout: Duration) -> Result<serde_json::Value, RemoteError> {
        self.transport().map_err(RemoteError::Unreachable)?.daemons.call(host, method, params, timeout)
    }
    fn starting(&self, host: &str) -> bool {
        // Looks only at a transport that is already there: finding the CLI can take a while.
        let Ok(cached) = self.transport.try_lock() else {
            return false;
        };
        cached
            .as_ref()
            .is_some_and(|transport| transport.daemons.starting(host))
    }
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
    fn ssh_routes(&self) -> Result<Vec<SshRoute>, String> {
        Ok(self
            .transport()?
            .cli
            .ssh_routes()?
            .into_iter()
            .map(|(host, target)| SshRoute {
                id: host.id,
                label: host.label,
                target,
            })
            .collect())
    }
    fn add_ssh(&self, request: &AddSshRequest) -> Result<(), String> {
        self.transport()?.cli.add_ssh(
            &request.target,
            &request.label,
            request.remote_bin.as_deref(),
            request.somtus.as_deref(),
        )
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
    /// What the file says the next launch reopens; see `launch_record`.
    saved_selection: Option<String>,
    observers: BTreeMap<u64, RemoteWants>,
    next_observer: u64,
    polling: bool,
    defaults: RemoteDefaults,
}
impl State {
    /// What the next launch reopens: the selection, unless the registry has dropped its host. The
    /// selection itself stays, because a window on that project keeps showing why it is empty.
    fn launch_record(&self) -> Option<String> {
        self.selected.clone().filter(|key| {
            !tree::parse_project_key(key).is_some_and(|(host, _)| self.tree.host_removed(host))
        })
    }
    /// Whether the file should be written again, which may be so when nothing drawn changed.
    fn save_due(&mut self) -> bool {
        self.tree.take_kept_dirty() || self.launch_record() != self.saved_selection
    }
    fn begin_polling(&mut self) -> bool {
        if self.polling || !self.observers.values().any(RemoteWants::needs_polling) {
            return false;
        }
        self.polling = true;
        true
    }
    fn plan(&mut self, now: Instant) -> Vec<Request> {
        let wants: Vec<_> = self
            .observers
            .values()
            .filter_map(|w| w.engine().ok())
            .collect();
        wants
            .into_iter()
            .flat_map(|wants| self.tree.plan(now, &wants))
            .collect()
    }
}
/// Threads that run planned requests, at most. Each request costs a socket and often a process, on
/// this Mac while no daemon is up and on the host for each list. One thread per request made all of
/// that grow with the hosts: 64 of them reach about 320 sockets at once, past the 256 descriptors
/// a launch from the Dock gets, and then local terminals and healthy hosts fail too.
const MAX_WORKERS: usize = 16;
/// Of those, threads on lists. A list can take 25 s on a slow host, and the rest of the threads are
/// there for what is cheap and what every window waits for: the state of each link and the registry.
const MAX_LIST_WORKERS: usize = 12;
/// One host's lists that run at once. The host itself runs no more reads at a time (`SHARED_SLOTS`
/// in `remote/src/lanes.rs`), so any more would hold a thread just to queue there, and then a few
/// slow hosts could keep every other host's lists waiting.
const MAX_LISTS_PER_HOST: usize = 3;

/// The host a list is for. The registry and a link's status are not lists.
fn list_host(request: &Request) -> Option<&str> {
    match request {
        Request::Hosts | Request::Status { .. } => None,
        Request::Projects { host }
        | Request::Orchestrators { host }
        | Request::Worktrees { host, .. }
        | Request::Tasks { host, .. }
        | Request::Shells { host, .. } => Some(host),
    }
}

/// The planned requests that no worker has started yet, and who is at work. A request stays
/// marked as running in the tree from its plan until its answer, so each is here once at most.
#[derive(Default)]
struct WorkQueue {
    /// The registry and links' statuses, oldest first. They go before any list.
    first: VecDeque<Request>,
    /// Lists waiting by host, oldest first.
    lists: BTreeMap<String, VecDeque<Request>>,
    /// Lists running by host. A host with none has no entry.
    running: BTreeMap<String, usize>,
    /// The host that had the last turn. The next goes to a host after it, so a host with a long
    /// list of figures to read takes turns with the others.
    last_host: String,
    workers: usize,
}
impl WorkQueue {
    fn push(&mut self, request: Request) {
        match list_host(&request).map(str::to_owned) {
            Some(host) => self.lists.entry(host).or_default().push_back(request),
            None => self.first.push_back(request),
        }
    }
    /// The next request to start. A list for a host whose daemon is being started stays here, as a
    /// worker could only wait for that with it. A later tick finds it.
    fn take(&mut self, starting: impl Fn(&str) -> bool) -> Option<Request> {
        if let Some(request) = self.first.pop_front() {
            return Some(request);
        }
        if self.running.values().sum::<usize>() >= MAX_LIST_WORKERS {
            return None;
        }
        let ready = |host: &&String| {
            self.running
                .get(*host)
                .is_none_or(|lists| *lists < MAX_LISTS_PER_HOST)
                && !starting(host)
        };
        let host = self
            .lists
            .keys()
            .filter(|host| **host > self.last_host)
            .find(ready)
            .or_else(|| self.lists.keys().find(ready))?
            .clone();
        let waiting = self.lists.get_mut(&host)?;
        let request = waiting.pop_front()?;
        if waiting.is_empty() {
            self.lists.remove(&host);
        }
        *self.running.entry(host.clone()).or_default() += 1;
        self.last_host = host;
        Some(request)
    }
    /// A request from `take` has ended.
    fn finished(&mut self, request: &Request) {
        let Some(host) = list_host(request) else {
            return;
        };
        match self.running.get_mut(host) {
            Some(lists) if *lists > 1 => *lists -= 1,
            _ => {
                self.running.remove(host);
            }
        }
    }
    /// A request from `take` that no worker could start goes back to the head of its line.
    fn give_back(&mut self, request: Request) {
        self.finished(&request);
        match list_host(&request).map(str::to_owned) {
            Some(host) => self.lists.entry(host).or_default().push_front(request),
            None => self.first.push_front(request),
        }
    }
    fn drain(&mut self) -> Vec<Request> {
        let lists = std::mem::take(&mut self.lists);
        self.first
            .drain(..)
            .chain(lists.into_values().flatten())
            .collect()
    }
}

pub struct RemoteService {
    home: PathBuf,
    backend: Arc<dyn RemoteBackend>,
    state: Mutex<State>,
    queue: Mutex<WorkQueue>,
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
                saved_selection: kept.selected.clone(),
                selected: kept.selected,
                observers: BTreeMap::new(),
                next_observer: 0,
                polling: false,
                defaults: RemoteDefaults {
                    routes: home
                        .join("remote/relay-routes.json")
                        .to_string_lossy()
                        .into_owned(),
                    ..Default::default()
                },
            }),
            queue: Mutex::default(),
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
        if state.save_due() {
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
            let selected = state.launch_record();
            state.saved_selection = selected.clone();
            Kept {
                selected,
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
        self.state.lock().unwrap_or_else(PoisonError::into_inner).plan(now)
    }
    fn polling_plan(&self, now: Instant) -> Option<Vec<Request>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.observers.values().any(RemoteWants::needs_polling) {
            // This decision and a new subscription's start claim share the
            // observer lock, so leaving an idle loop cannot lose a restart.
            state.polling = false;
            return None;
        }
        Some(state.plan(now))
    }
    pub(crate) fn poll(self: &Arc<Self>) {
        self.dispatch(self.plan(Instant::now()));
    }
    /// Hands planned requests to the workers, starting more of them while there is work they could
    /// start and room for them. Mutations and what a person asks for directly never come here.
    fn dispatch(self: &Arc<Self>, requests: Vec<Request>) {
        self.dispatch_with(requests, |work| {
            thread::Builder::new()
                .name("somtus-remote-read".into())
                .spawn(work)
                .map(drop)
        });
    }
    fn dispatch_with(
        self: &Arc<Self>,
        requests: Vec<Request>,
        spawn: impl Fn(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
    ) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        for request in requests {
            queue.push(request);
        }
        while queue.workers < MAX_WORKERS {
            let Some(request) = queue.take(|host| self.backend.starting(host)) else {
                return;
            };
            let service = Arc::downgrade(self);
            let assigned = request.clone();
            match spawn(Box::new(move || Self::work(service, assigned))) {
                // Under this lock, so that it cannot be done before it has been counted.
                Ok(()) => queue.workers += 1,
                Err(error) => {
                    queue.give_back(request);
                    // A worker that is running takes what waits here, and the next tick tries again.
                    if queue.workers == 0 {
                        // Nobody will take these, and a request marked as running that is never
                        // answered is never asked again.
                        let stranded = queue.drain();
                        drop(queue);
                        let error = format!("This Mac could not start another thread: {error}");
                        for request in stranded {
                            self.take_reply(&request, failed(&request, error.clone()));
                        }
                    }
                    return;
                }
            }
        }
    }
    /// One worker: the request it was started for, then whatever is next, until nothing can start.
    /// Only the request it is running keeps the service alive.
    fn work(service: Weak<Self>, mut request: Request) {
        while let Some(owner) = service.upgrade() {
            owner.run_request(&request);
            let mut queue = owner.queue.lock().unwrap_or_else(PoisonError::into_inner);
            queue.finished(&request);
            match queue.take(|host| owner.backend.starting(host)) {
                Some(next) => request = next,
                None => {
                    queue.workers -= 1;
                    return;
                }
            }
        }
    }
    fn run_request(&self, request: &Request) {
        // A panic must not cost the set a worker for good, nor leave the request marked as running.
        let attempt = |reply: &dyn Fn() -> Reply| {
            catch_unwind(AssertUnwindSafe(|| self.take_reply(request, reply())))
        };
        if attempt(&|| self.backend.run(request)).is_err() {
            let _ = attempt(&|| failed(request, "The request failed unexpectedly.".into()));
        }
    }
    /// Takes in the answer to a planned request. Returns whether the windows were sent a snapshot:
    /// one holds every host's lists, and most answers leave them as they were.
    fn take_reply(&self, request: &Request, reply: Reply) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = state.tree.apply(request, reply);
        if changed {
            drop(state);
            self.publish();
        } else if state.save_due() {
            drop(state);
            self.save_cache();
        }
        changed
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
    pub(crate) fn ssh_routes(&self) -> Result<Vec<SshRoute>, String> {
        self.backend.ssh_routes()
    }
    pub(crate) fn add_ssh(&self, request: &AddSshRequest) -> Result<(), String> {
        self.register_host(|| self.backend.add_ssh(request))
    }
    pub fn add_host(&self, link: String, label: String) -> Result<(), String> {
        let link = check_link(&link)?;
        self.register_host(|| {
            self.backend
                .add_host(&link, label.trim())
                .map_err(|e| secret_error(&e, Some(&link)))
        })
    }
    fn register_host(&self, register: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        let _mutation = self
            .host_mutation
            .try_lock()
            .map_err(|_| "conflict: a remote host change is already running")?;
        register()?;
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
        // Its session titles leave the Agents view and the disk now, as the next registry read
        // may be a long way off. The route is gone either way, so a cache that cannot be written
        // must not look like a removal that failed.
        let _ = crate::hosts::forget_route(&self.home, &host);
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tree
            .invalidate_hosts();
        Ok(())
    }
    /// Whether a good registry read no longer lists this route, for a chat whose daemon cannot be
    /// started. Tabs can stay open on a removed host with no panel left to read the registry, so
    /// this reads it itself when nothing has for `tree::ROUTE_CHECK_INTERVAL`, however many ask.
    pub fn route_removed(&self, host: &str) -> bool {
        self.route_removed_at(host, Instant::now())
    }
    fn route_removed_at(&self, host: &str, now: Instant) -> bool {
        let read = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tree
            .begin_route_check(now);
        if read {
            // On the chat's own thread, behind the workers' guard: a panic must neither end the
            // chat's feed nor leave the read marked as running, after which no plan and no chat
            // would read the registry again.
            self.run_request(&Request::Hosts);
        }
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tree
            .host_removed(host)
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
    /// The live orchestrator of the selected remote project if one is listed;
    /// else the host is asked once. The desktop never starts one.
    pub fn orchestrator(&self, key: &str) -> Result<RemoteOrchestrator, String> {
        let (host, project) = remote_parts(key)?;
        let known = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tree
            .orchestrator(host, Some(project))
            .cloned();
        if let Some(shell) = known {
            return Ok(RemoteOrchestrator {
                shell: Some(shell.into()),
                notice: None,
            });
        }
        let request = Request::Orchestrators { host: host.into() };
        let reply = self.backend.run(&request);
        let listed = matches!(reply, Reply::Orchestrators(Ok(_)));
        let (shell, name) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.tree.apply(&request, reply);
            (
                state.tree.orchestrator(host, Some(project)).cloned(),
                state.tree.host_name(host),
            )
        };
        self.publish();
        Ok(match shell {
            Some(shell) => RemoteOrchestrator {
                shell: Some(shell.into()),
                notice: None,
            },
            None => RemoteOrchestrator {
                shell: None,
                notice: Some(if listed {
                    format!("{name} has no project orchestrator running")
                } else {
                    format!("Cannot reach {name}")
                }),
            },
        })
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
        // As the GPUI desktop: an agent follows this Mac's "Agent terminals run
        // unrestricted", read now so every window follows a change at once; a
        // plain shell never is. The request's own flag is not trusted.
        let unrestricted = kind != NewShellKind::Shell
            && riwork_core::settings::agent_terminals_unrestricted(&self.home);
        let result = self.backend.create_shell(host, &scope, kind, unrestricted);
        let uncertain = result.as_ref().err().is_some_and(tree::outcome_unknown);
        // The window's notice is the error's own message, as in GPUI; the
        // Shells panel keeps the project's fuller failure row.
        let message = result.as_ref().err().map(RemoteError::message);
        let shell = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tree
            .finish_shell(host, project, result);
        self.publish();
        shell.map(Into::into).ok_or_else(|| match message {
            Some(message) => format!("{}: {message}", if uncertain { "broken" } else { "refused" }),
            None => "broken: The host was removed while its reply arrived. Check its terminals before trying again.".into(),
        })
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
    // One engine sort per distinct order among windows showing Projects, so
    // each window's folders follow its own order (GPUI's one app-wide order is
    // the case where they all agree).
    let mut orders: Vec<_> = state
        .observers
        .values()
        .filter(|w| w.folders)
        .map(|w| (w.order.clone(), w.descending, w.project_order()))
        .collect();
    orders.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    orders.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    let ordered: Vec<_> = orders
        .into_iter()
        .map(|(order, descending, by)| {
            let keys: BTreeMap<_, _> = state
                .tree
                .folders("", by, &HashSet::new(), None)
                .into_iter()
                .map(|folder| {
                    (
                        folder.host,
                        folder
                            .projects
                            .into_iter()
                            .map(|p| p.key)
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            (order, descending, keys)
        })
        .collect();
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
            orders: ordered
                .iter()
                .filter_map(|(order, descending, keys)| {
                    Some(RemoteFolderOrder {
                        order: order.clone(),
                        descending: *descending,
                        keys: keys.get(&folder.host)?.clone(),
                    })
                })
                .collect(),
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
                    live: state
                        .tree
                        .live_shells(&view.host, &view.project)
                        .map(|live| live as u32),
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
    let mut tabs: Vec<_> = state
        .observers
        .values()
        .flat_map(|w| w.tabs.iter().cloned())
        .collect();
    tabs.sort();
    tabs.dedup();
    let titles = tabs
        .into_iter()
        .filter_map(|key| {
            let (host, shell) = key.strip_prefix("remote:")?.split_once(':')?;
            let title = state.tree.tab_title(host, shell);
            Some(RemoteTabTitle { key, title })
        })
        .collect();
    RemoteSnapshot {
        schema: 1,
        titles,
        hosts: state
            .tree
            .hosts()
            .map(|(host, link)| RemoteHostDto {
                id: host.id.clone(),
                label: host.label.clone(),
                link: link_name(link),
                status: state.tree.link_text(&host.id),
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
    if !(link.starts_with("somtus://pair?") || link.starts_with("riwork://pair?")) || link.contains(char::is_whitespace) {
        return Err("invalid: That is not a Somtus pairing link.".into());
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
            if part.contains("somtus://pair?") || part.contains("riwork://pair?") {
                "[pairing link hidden]"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl RemoteService {
    /// For the first window of an ordinary launch: asking for a folder chooses
    /// a project of this Mac, so the record is cleared and nothing reopens;
    /// otherwise the remote project the last window selected, if valid.
    pub fn launch_selection(&self, folder: bool) -> Option<String> {
        if folder {
            let _ = self.select(None);
            return None;
        }
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .launch_record()
            .filter(|key| remote_parts(key).is_ok())
    }
}
/// `RemoteService::launch_selection` on the process service, before any AppView.
pub fn launch_selection(folder: bool) -> Option<String> {
    process().ok()?.launch_selection(folder)
}

static PROCESS: OnceLock<Arc<RemoteService>> = OnceLock::new();
/// Shared daemon owner for native features; no transport is owned by a view.
pub(crate) fn call_host(host: &str, method: &str, params: serde_json::Value, timeout: Duration) -> Result<serde_json::Value, RemoteError> {
    identifier(host).map_err(RemoteError::Protocol)?;
    process().map_err(RemoteError::Unreachable)?.backend.call(host, method, params, timeout)
}
/// The daemon's view of a host's link, through the same seam as `call_host`. Like a read, it
/// starts the daemon when none is running.
pub(crate) fn host_status(host: &str) -> Result<HostStatus, String> {
    identifier(host)?;
    match process()?.backend.run(&Request::Status { host: host.into() }) {
        Reply::Status(status) => status,
        _ => Err("Unexpected answer to a status request".into()),
    }
}
/// `RemoteService::route_removed` on the process service. Without a service nothing is known, so
/// nothing counts as removed.
pub(crate) fn route_removed(host: &str) -> bool {
    process().is_ok_and(|remote| remote.route_removed(host))
}
pub fn install_service(remote: Arc<RemoteService>) -> Result<(), String> {
    PROCESS
        .set(remote)
        .map_err(|_| "conflict: remote service is already installed".into())
}
pub(crate) fn process() -> Result<Arc<RemoteService>, String> {
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
fn start_polling(remote: &Arc<RemoteService>) -> Result<(), String> {
    start_polling_with(remote, || thread::sleep(Duration::from_millis(500))).map(|_| ())
}

fn start_polling_with(
    remote: &Arc<RemoteService>,
    wait: impl FnMut() + Send + 'static,
) -> Result<Option<thread::JoinHandle<()>>, String> {
    // Each subscription owns only a lease. Claim one worker under the observer
    // lock; it exits when no lease needs polling. Cached-title subscriptions
    // remain live, and a later active lease can restart the polling loop.
    let mut state = remote.state.lock().unwrap_or_else(PoisonError::into_inner);
    if !state.begin_polling() {
        return Ok(None);
    }
    // Keep the start claim serialized through thread creation too. On a spawn
    // error, another subscriber must see the cleared claim and be able to retry.
    let weak = Arc::downgrade(remote);
    match thread::Builder::new()
        .name("somtus-remote-poll".into())
        .spawn(move || run_polling(weak, wait))
    {
        Ok(worker) => Ok(Some(worker)),
        Err(error) => {
            state.polling = false;
            Err(error.to_string())
        }
    }
}

fn run_polling(remote: Weak<RemoteService>, mut wait: impl FnMut()) {
    loop {
        let Some(service) = remote.upgrade() else { break };
        let Some(requests) = service.polling_plan(Instant::now()) else { break };
        service.dispatch(requests);
        // The wait never keeps the service alive. With no polling demand, the next
        // iteration exits without another timed wait or empty planning pass.
        drop(service);
        wait();
    }
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
        listener: Callback<crate::hub::SharedSnapshot<RemoteSnapshot>>,
        cx: CallCx<'_>,
    ) -> Result<Subscription, String> {
        wants.engine()?;
        Ok(Subscription::start(cx, async move {
            if let Ok((lease, topic)) = service::blocking(move || {
                let remote = process()?;
                let observation = remote.observe(wants)?;
                start_polling(&remote)?;
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
    /// ⌘⌥O and P·ORCH on a project of another Mac.
    pub async fn orchestrator(key: String) -> Result<RemoteOrchestrator, String> {
        service::blocking(move || process()?.orchestrator(&key)).await
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
        /// Answer `orchestrators.list` with an empty list instead of failing.
        orchestrators_listed: std::sync::atomic::AtomicBool,
        /// What the registry answers. By default it lists the fixture's one host.
        registry: Mutex<Option<Result<Vec<Host>, String>>>,
        /// What a status request answers. By default the link is online.
        link: Mutex<Option<Result<HostStatus, String>>>,
        /// How a creation fails. By default no answer comes.
        create_error: Mutex<Option<RemoteError>>,
        removal_fails: std::sync::atomic::AtomicBool,
    }
    impl Fake {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                creates: AtomicUsize::new(0),
                orchestrators_listed: Default::default(),
                registry: Default::default(),
                link: Default::default(),
                create_error: Default::default(),
                removal_fails: Default::default(),
            })
        }
    }
    impl RemoteBackend for Fake {
        fn run(&self, request: &Request) -> Reply {
            self.calls.lock().unwrap().push(format!("{request:?}"));
            match request {
                Request::Hosts => Reply::Hosts(
                    self.registry.lock().unwrap().clone().unwrap_or_else(|| {
                        Ok(vec![Host {
                            id: HOST.into(),
                            label: "Disposable Mac".into(),
                        }])
                    }),
                ),
                Request::Status { .. } => Reply::Status(
                    self.link
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or_else(|| Ok(link_state(LinkState::Online, None))),
                ),
                Request::Projects { .. } => Reply::Projects(Ok(vec![tree::RemoteProject {
                    id: PROJECT.into(),
                    name: "Fixture project".into(),
                    created_at: 1,
                }])),
                Request::Orchestrators { .. }
                    if self.orchestrators_listed.load(Ordering::SeqCst) =>
                {
                    Reply::Orchestrators(Ok(Vec::new()))
                }
                _ => failed(request, "fixture list not supplied".into()),
            }
        }
        fn add_host(&self, link: &str, _: &str) -> Result<(), String> {
            Err(format!("fixture rejected {link}"))
        }
        fn remove_host(&self, host: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("remove:{host}"));
            if self.removal_fails.load(Ordering::SeqCst) {
                return Err("the registry is locked".into());
            }
            Ok(())
        }
        fn pair_desktop(&self, _: &PairRequest) -> Result<String, String> {
            Ok("somtus://pair?DISPOSABLE_FAKE_NO_CREDENTIAL".into())
        }
        fn create_project(&self, _: &str, _: &str) -> Result<tree::RemoteProject, RemoteError> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            Err(self.create_failure())
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
            Err(self.create_failure())
        }
    }
    impl Fake {
        fn create_failure(&self) -> RemoteError {
            self.create_error
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(RemoteError::Timeout)
        }
    }
    fn link_state(state: LinkState, reason: Option<&str>) -> HostStatus {
        HostStatus {
            state,
            rtt_ms: None,
            since: None,
            reason: reason.map(str::to_owned),
            label: None,
            cause: None,
            needs_action: false,
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
    fn controlled_polling(remote: &Arc<RemoteService>) -> (
        thread::JoinHandle<()>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (waiting, waits) = std::sync::mpsc::channel();
        let (resume, resumes) = std::sync::mpsc::channel();
        let worker = start_polling_with(remote, move || {
            waiting.send(()).unwrap();
            resumes.recv().expect("test released each expected wait");
        }).unwrap().expect("active demand starts one worker");
        (worker, waits, resume)
    }
    fn wait_for_poll(waits: &std::sync::mpsc::Receiver<()>) {
        // Only a deadlock guard; correctness uses explicit phase handshakes,
        // not elapsed-time assertions or an assumed scheduling speed.
        waits.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    /// Who is running, as a backend that holds requests sees it.
    #[derive(Default)]
    struct Load {
        seen: Vec<Request>,
        running: usize,
        peak: usize,
        lists: BTreeMap<String, usize>,
        peak_lists: usize,
        peak_host: usize,
        released: bool,
    }
    /// A backend that keeps the requests `hold` picks running until the test lets them go.
    struct Held {
        load: Mutex<Load>,
        moved: std::sync::Condvar,
        hold: fn(&Request) -> bool,
        starting: Mutex<HashSet<String>>,
        panics: std::sync::atomic::AtomicBool,
    }
    impl RemoteBackend for Held {
        fn run(&self, request: &Request) -> Reply {
            assert!(!self.panics.load(Ordering::SeqCst), "a backend that panics");
            let host = list_host(request);
            let mut load = self.load.lock().unwrap();
            load.seen.push(request.clone());
            load.running += 1;
            load.peak = load.peak.max(load.running);
            if let Some(host) = host {
                *load.lists.entry(host.to_owned()).or_default() += 1;
                let lists = load.lists.values().sum();
                let of_host = load.lists[host];
                load.peak_lists = load.peak_lists.max(lists);
                load.peak_host = load.peak_host.max(of_host);
            }
            self.moved.notify_all();
            while (self.hold)(request) && !load.released {
                load = self.moved.wait(load).unwrap();
            }
            load.running -= 1;
            if let Some(host) = host {
                *load.lists.get_mut(host).unwrap() -= 1;
            }
            self.moved.notify_all();
            failed(request, "the fixture does not answer".into())
        }
        fn starting(&self, host: &str) -> bool {
            self.starting.lock().unwrap().contains(host)
        }
        fn add_host(&self, _: &str, _: &str) -> Result<(), String> {
            Err("not in this fixture".into())
        }
        fn remove_host(&self, _: &str) -> Result<(), String> {
            Err("not in this fixture".into())
        }
        fn pair_desktop(&self, _: &PairRequest) -> Result<String, String> {
            Err("not in this fixture".into())
        }
        fn create_project(&self, _: &str, _: &str) -> Result<tree::RemoteProject, RemoteError> {
            Err(RemoteError::Timeout)
        }
        fn create_shell(
            &self,
            _: &str,
            _: &ShellScope,
            _: NewShellKind,
            _: bool,
        ) -> Result<tree::RemoteShell, RemoteError> {
            Err(RemoteError::Timeout)
        }
    }
    impl Held {
        fn wait_for(&self, what: &str, done: impl Fn(&Load) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut load = self.load.lock().unwrap();
            while !done(&load) {
                let left = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !left.is_zero(),
                    "still waiting for {what}: {} running, {} seen",
                    load.running,
                    load.seen.len()
                );
                load = self.moved.wait_timeout(load, left).unwrap().0;
            }
        }
        fn release(&self) {
            self.load.lock().unwrap().released = true;
            self.moved.notify_all();
        }
    }
    fn held_fixture(
        hold: fn(&Request) -> bool,
    ) -> (tempfile::TempDir, Arc<RemoteService>, Arc<Held>) {
        let directory = tempfile::tempdir().unwrap();
        let held = Arc::new(Held {
            load: Mutex::default(),
            moved: Default::default(),
            hold,
            starting: Mutex::default(),
            panics: Default::default(),
        });
        let remote = RemoteService::new(directory.path().into(), held.clone());
        (directory, remote, held)
    }
    fn lists_of(host: &str) -> [Request; 4] {
        let (host, project) = (host.to_owned(), PROJECT.to_owned());
        [
            Request::Projects { host: host.clone() },
            Request::Worktrees {
                host: host.clone(),
                project: project.clone(),
            },
            Request::Tasks {
                host: host.clone(),
                project: project.clone(),
            },
            Request::Shells { host, project },
        ]
    }
    fn workers(remote: &RemoteService) -> usize {
        remote.queue.lock().unwrap().workers
    }
    /// Every worker has gone, and with it what it held of the service.
    fn until_idle(remote: &Arc<RemoteService>) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while workers(remote) > 0 || Arc::strong_count(remote) > 1 {
            assert!(
                Instant::now() < deadline,
                "{} workers and {} owners left",
                workers(remote),
                Arc::strong_count(remote)
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn no_more_threads(_: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
        Err(std::io::Error::other("no more threads"))
    }

    #[test]
    fn requests_run_on_a_bounded_set_of_workers_that_ends_with_the_work() {
        let (_directory, remote, held) = held_fixture(|_| true);
        // As many hosts as the registry allows, each with its link and its lists due.
        let mut requests = Vec::new();
        for n in 0..64 {
            let host = format!("host-{n:02}");
            requests.push(Request::Status { host: host.clone() });
            requests.extend(lists_of(&host));
        }
        let total = requests.len();
        remote.dispatch(requests);
        held.wait_for("a full set", |load| load.running == MAX_WORKERS);
        // The poll worker comes by on every tick, and that adds nobody.
        for _ in 0..3 {
            remote.dispatch(Vec::new());
        }
        thread::sleep(Duration::from_millis(50));
        assert_eq!(held.load.lock().unwrap().running, MAX_WORKERS);
        assert_eq!(workers(&remote), MAX_WORKERS);

        held.release();
        held.wait_for("every request", |load| {
            load.seen.len() == total && load.running == 0
        });
        {
            let load = held.load.lock().unwrap();
            assert_eq!(load.peak, MAX_WORKERS);
            assert!(load.peak_lists <= MAX_LIST_WORKERS, "{}", load.peak_lists);
            assert!(load.peak_host <= MAX_LISTS_PER_HOST, "{}", load.peak_host);
            let mut asked: Vec<_> = load.seen.iter().map(|r| format!("{r:?}")).collect();
            asked.sort();
            asked.dedup();
            assert_eq!(asked.len(), total, "each one once");
        }
        // Nobody stays on for more work, or keeps the service alive.
        until_idle(&remote);
    }

    #[test]
    fn a_status_and_the_registry_never_queue_behind_lists() {
        let (_directory, remote, held) = held_fixture(|request| list_host(request).is_some());
        let lists: Vec<_> = (0..8)
            .flat_map(|n| lists_of(&format!("host-{n}")))
            .collect();
        let total = lists.len() + 2;
        remote.dispatch(lists);
        held.wait_for("the lists' share of the workers", |load| {
            load.running == MAX_LIST_WORKERS
        });
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            held.load.lock().unwrap().running,
            MAX_LIST_WORKERS,
            "the rest of the workers stay free"
        );

        let status = Request::Status {
            host: "host-0".into(),
        };
        remote.dispatch(vec![status.clone(), Request::Hosts]);
        held.wait_for("the status and the registry", |load| {
            load.seen.contains(&status) && load.seen.contains(&Request::Hosts)
        });

        held.release();
        held.wait_for("every list", |load| {
            load.seen.len() == total && load.running == 0
        });
        until_idle(&remote);
    }

    #[test]
    fn statuses_come_first_and_hosts_take_turns_with_their_lists() {
        let mut queue = WorkQueue::default();
        let [a_projects, a_worktrees, a_tasks, a_shells] = lists_of("a");
        let [b_projects, ..] = lists_of("b");
        let [c_projects, ..] = lists_of("c");
        let status = Request::Status { host: "z".into() };
        for request in [
            a_projects.clone(),
            a_worktrees.clone(),
            a_tasks.clone(),
            a_shells.clone(),
            b_projects.clone(),
            c_projects.clone(),
            status.clone(),
            Request::Hosts,
        ] {
            queue.push(request);
        }
        let mut order = Vec::new();
        while let Some(request) = queue.take(|_| false) {
            queue.finished(&request);
            order.push(request);
        }
        assert_eq!(
            order,
            [
                status,
                Request::Hosts,
                a_projects.clone(),
                b_projects.clone(),
                c_projects,
                a_worktrees,
                a_tasks,
                a_shells
            ]
        );
        // The turn carries over to what is planned next: `a` went last, so `b` is next.
        queue.push(a_projects);
        queue.push(b_projects.clone());
        assert_eq!(queue.take(|_| false), Some(b_projects));
    }

    #[test]
    fn a_host_holds_only_a_few_list_workers_and_all_hosts_twelve() {
        let mut queue = WorkQueue::default();
        // Two lists more than one host may run at once, and nothing else waiting.
        let [projects, worktrees, tasks, shells] = lists_of("busy");
        for request in [projects, worktrees, tasks, shells] {
            queue.push(request);
        }
        let mut taken = Vec::new();
        while let Some(request) = queue.take(|_| false) {
            taken.push(request);
        }
        assert_eq!(taken.len(), MAX_LISTS_PER_HOST);
        // One ending lets the next start.
        queue.finished(&taken.pop().unwrap());
        assert!(queue.take(|_| false).is_some());
        assert!(queue.take(|_| false).is_none());

        let mut queue = WorkQueue::default();
        for n in 0..6 {
            for request in lists_of(&format!("host-{n}")) {
                queue.push(request);
            }
        }
        let mut taken = Vec::new();
        while let Some(request) = queue.take(|_| false) {
            taken.push(request);
        }
        assert_eq!(taken.len(), MAX_LIST_WORKERS);
        // What goes first still goes.
        queue.push(Request::Hosts);
        assert_eq!(queue.take(|_| false), Some(Request::Hosts));
        queue.finished(&taken.pop().unwrap());
        assert!(queue.take(|_| false).is_some());
        assert!(queue.take(|_| false).is_none());
    }

    #[test]
    fn slow_hosts_leave_workers_for_the_others() {
        let (_directory, remote, held) =
            held_fixture(|request| list_host(request).is_some_and(|host| host.starts_with("slow")));
        let slow: Vec<_> = (0..3)
            .flat_map(|n| lists_of(&format!("slow-{n}")))
            .collect();
        let total = slow.len() + 4;
        remote.dispatch(slow);
        held.wait_for("the slow hosts' lists", |load| {
            load.running == 3 * MAX_LISTS_PER_HOST
        });
        thread::sleep(Duration::from_millis(50));
        assert_eq!(held.load.lock().unwrap().running, 3 * MAX_LISTS_PER_HOST);
        // All they can hold, and another host goes straight through.
        remote.dispatch(lists_of("quick").to_vec());
        held.wait_for("the quick host's lists", |load| {
            lists_of("quick")
                .iter()
                .all(|list| load.seen.contains(list))
        });
        held.release();
        held.wait_for("every list", |load| {
            load.seen.len() == total && load.running == 0
        });
        assert!(held.load.lock().unwrap().peak_host <= MAX_LISTS_PER_HOST);
        until_idle(&remote);
    }

    #[test]
    fn a_list_for_a_host_whose_daemon_is_starting_holds_no_worker() {
        let (_directory, remote, held) = held_fixture(|_| false);
        held.starting.lock().unwrap().insert("starting".into());
        let mut lists = lists_of("starting").to_vec();
        lists.extend(lists_of("other"));
        remote.dispatch(lists);
        held.wait_for("the other host's lists", |load| load.seen.len() == 4);
        until_idle(&remote);
        // Nobody sits waiting on that daemon, nor are its lists lost or answered.
        assert!(
            held.load
                .lock()
                .unwrap()
                .seen
                .iter()
                .all(|request| list_host(request) == Some("other"))
        );
        // A tick while it is still at it starts nothing.
        remote.dispatch(Vec::new());
        assert_eq!(workers(&remote), 0);
        // A status does go: it does not wait for the daemon, and says the link is being made.
        let status = Request::Status {
            host: "starting".into(),
        };
        remote.dispatch(vec![status.clone()]);
        held.wait_for("the status", |load| load.seen.contains(&status));
        until_idle(&remote);

        held.starting.lock().unwrap().clear();
        remote.dispatch(Vec::new());
        held.wait_for("the lists held back", |load| load.seen.len() == 9);
        until_idle(&remote);
    }

    #[test]
    fn when_no_worker_can_start_every_request_is_answered_and_asked_again() {
        let (_directory, remote, fake) = fixture();
        let (_observation, _topic) = remote
            .observe(RemoteWants {
                settings: true,
                ..Default::default()
            })
            .unwrap();
        let start = Instant::now();
        let planned = remote.plan(start);
        assert_eq!(planned, [Request::Hosts]);
        remote.dispatch_with(planned, no_more_threads);
        let data = remote.current();
        assert!(
            data.hosts_error
                .as_deref()
                .unwrap()
                .contains("no more threads"),
            "{data:?}"
        );
        assert!(!remote.hub.publish(data), "the windows were told");
        assert_eq!(workers(&remote), 0);
        assert!(fake.calls.lock().unwrap().is_empty());
        // Asked again when due, as a request still marked as running never would be.
        let again = start + tree::HOSTS_INTERVAL;
        assert_eq!(remote.plan(again), [Request::Hosts]);
        answer(&remote, Request::Hosts);

        // And with a few at once: each gets its answer.
        let later = again + tree::HOSTS_INTERVAL;
        let planned = remote.plan(later);
        assert_eq!(
            planned,
            [Request::Hosts, Request::Status { host: HOST.into() }]
        );
        remote.dispatch_with(planned, no_more_threads);
        let data = remote.current();
        assert!(data.hosts[0].status.contains("no more threads"), "{data:?}");
        assert!(
            data.hosts_error
                .as_deref()
                .unwrap()
                .contains("no more threads")
        );
        let next = later + tree::HOSTS_INTERVAL;
        assert_eq!(
            remote.plan(next),
            [Request::Hosts, Request::Status { host: HOST.into() }]
        );
    }

    #[test]
    fn a_worker_that_is_running_takes_over_what_could_not_get_its_own() {
        let (_directory, remote, held) = held_fixture(|_| true);
        let spawned = AtomicUsize::new(0);
        let requests = lists_of("host").to_vec();
        let total = requests.len();
        remote.dispatch_with(requests, |work| {
            if spawned.fetch_add(1, Ordering::SeqCst) == 0 {
                thread::Builder::new().spawn(work).map(drop)
            } else {
                no_more_threads(work)
            }
        });
        held.wait_for("the one worker", |load| load.running == 1);
        assert_eq!(workers(&remote), 1);
        // Nothing was given up on while it can still be done.
        assert_eq!(held.load.lock().unwrap().seen.len(), 1);
        held.release();
        held.wait_for("every list", |load| {
            load.seen.len() == total && load.running == 0
        });
        until_idle(&remote);
    }

    #[test]
    fn a_request_that_panics_costs_no_worker_and_is_asked_again() {
        let (_directory, remote, held) = held_fixture(|_| false);
        held.panics.store(true, Ordering::SeqCst);
        let (_observation, _topic) = remote
            .observe(RemoteWants {
                settings: true,
                ..Default::default()
            })
            .unwrap();
        let start = Instant::now();
        remote.dispatch(remote.plan(start));
        let deadline = Instant::now() + Duration::from_secs(20);
        while remote.current().hosts_error.is_none() {
            assert!(Instant::now() < deadline, "the request stayed out");
            thread::sleep(Duration::from_millis(2));
        }
        until_idle(&remote);
        assert_eq!(remote.plan(start + tree::HOSTS_INTERVAL), [Request::Hosts]);
    }

    #[test]
    fn a_registry_read_that_panics_for_a_chat_is_answered_and_asked_again() {
        let (_directory, remote, held) = held_fixture(|_| false);
        held.panics.store(true, Ordering::SeqCst);
        let (_observation, _topic) = remote
            .observe(RemoteWants {
                settings: true,
                ..Default::default()
            })
            .unwrap();
        let start = Instant::now();
        // Asked on a chat's own thread: the panic ends here, as a read that failed, and the chat
        // goes on.
        let removed = catch_unwind(AssertUnwindSafe(|| remote.route_removed_at(HOST, start)));
        assert_eq!(removed.ok(), Some(false), "the panic reached the chat");
        assert!(remote.current().hosts_error.is_some());
        // Nor is the read left marked as running, so the registry is read again in its time.
        held.panics.store(false, Ordering::SeqCst);
        assert_eq!(remote.plan(start + tree::HOSTS_INTERVAL), [Request::Hosts]);
    }

    #[test]
    fn sixty_four_hosts_all_come_online_through_the_workers() {
        let (_directory, remote, fake) = fixture();
        *fake.registry.lock().unwrap() = Some(Ok((0..64)
            .map(|n| Host {
                id: format!("host-{n:02}"),
                label: format!("Mac {n}"),
            })
            .collect()));
        let (_observation, _topic) = remote
            .observe(RemoteWants {
                folders: true,
                settings: true,
                ..Default::default()
            })
            .unwrap();
        // Half-second ticks, as fast as the machine goes.
        let mut now = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            remote.dispatch(remote.plan(now));
            let data = remote.current();
            if data.hosts.len() == 64
                && data.folders.iter().all(|folder| {
                    folder.link.as_deref() == Some("online") && !folder.projects.is_empty()
                })
            {
                break;
            }
            assert!(Instant::now() < deadline, "{data:?}");
            now += Duration::from_millis(500);
            thread::sleep(Duration::from_millis(5));
        }
        until_idle(&remote);
        // What the windows hold is what is true, though most answers published nothing.
        assert!(!remote.hub.publish(remote.current()));
    }

    #[test]
    fn an_answer_that_changes_nothing_builds_no_snapshot() {
        let (_directory, remote, _fake) = fixture();
        let listed = || {
            Reply::Hosts(Ok(vec![Host {
                id: HOST.into(),
                label: "Disposable Mac".into(),
            }]))
        };
        assert!(remote.take_reply(&Request::Hosts, listed()));
        assert!(!remote.take_reply(&Request::Hosts, listed()));
        let request = Request::Status { host: HOST.into() };
        let online = |rtt| {
            Reply::Status(Ok(HostStatus {
                rtt_ms: Some(rtt),
                ..link_state(LinkState::Online, None)
            }))
        };
        assert!(remote.take_reply(&request, online(10)));
        // The round-trip time moves on every poll and is not drawn.
        assert!(!remote.take_reply(&request, online(20)));
        assert!(!remote.hub.publish(remote.current()));
        // A link that goes down is news.
        let down = || Reply::Status(Err("the daemon is gone".into()));
        assert!(remote.take_reply(&request, down()));
        assert!(!remote.take_reply(&request, down()));
        assert!(!remote.hub.publish(remote.current()));
    }

    #[test]
    fn a_first_empty_registry_answer_reaches_the_windows_and_the_file() {
        let (directory, remote, _fake) = fixture();
        let key = tree::project_key(HOST, PROJECT);
        remote.select(Some(key.clone())).unwrap();
        assert_eq!(
            remote_cache::load(directory.path()).selected.as_deref(),
            Some(key.as_str())
        );
        assert!(remote.take_reply(&Request::Hosts, Reply::Hosts(Ok(Vec::new()))));
        assert!(remote.current().projects.iter().any(|lists| lists.unpaired));
        assert!(!remote.hub.publish(remote.current()));
        assert_eq!(remote_cache::load(directory.path()).selected, None);
    }

    #[test]
    fn an_answer_that_changes_only_the_file_still_writes_it() {
        let directory = tempfile::tempdir().unwrap();
        let name = |id: &str| tree::RemoteProject {
            id: id.into(),
            name: format!("the name of {id}"),
            created_at: 1,
        };
        remote_cache::save(
            directory.path(),
            &Kept {
                selected: None,
                hosts: BTreeMap::from([
                    (HOST.to_owned(), vec![name("kept")]),
                    ("gone".to_owned(), vec![name("forgotten")]),
                ]),
            },
        )
        .unwrap();
        let remote = RemoteService::new(directory.path().into(), Fake::new());
        let listed = || {
            vec![Host {
                id: HOST.into(),
                label: "Disposable Mac".into(),
            }]
        };
        // The registry is already known to name only the one, and the file still has both.
        remote.state.lock().unwrap().tree.set_hosts(listed());
        assert!(
            remote_cache::load(directory.path())
                .hosts
                .contains_key("gone")
        );
        assert!(!remote.take_reply(&Request::Hosts, Reply::Hosts(Ok(listed()))));
        let kept = remote_cache::load(directory.path());
        assert!(!kept.hosts.contains_key("gone"));
        assert!(kept.hosts.contains_key(HOST));
    }

    #[test]
    fn polling_demand_matches_engine_requests_including_cached_titles() {
        for flags in 0..256 {
            let wants = RemoteWants {
                folders: flags & 1 != 0,
                settings: flags & 2 != 0,
                tab_hosts: if flags & 4 != 0 { vec![HOST.into()] } else { Vec::new() },
                project_key: (flags & 8 != 0).then(|| tree::project_key(HOST, PROJECT)),
                worktrees: flags & 16 != 0,
                tasks: flags & 32 != 0,
                shells: flags & 64 != 0,
                tabs: if flags & 128 != 0 { vec![format!("remote:{HOST}:shell")] } else { Vec::new() },
                ..Default::default()
            };
            let requests = RemoteTree::default().plan(Instant::now(), &wants.engine().unwrap());
            assert_eq!(wants.needs_polling(), !requests.is_empty(), "flags={flags}");
        }
    }
    #[test]
    fn polling_stops_when_idle_and_resubscription_cannot_lose_its_worker() {
        let (_directory, remote, fake) = fixture();
        assert!(start_polling_with(&remote, || panic!("unobserved worker"))
            .unwrap().is_none(), "an unobserved service starts no worker");
        let (_titles, _cached_topic) = remote.observe(RemoteWants {
            tabs: vec![format!("remote:{HOST}:shell")], ..Default::default()
        }).unwrap();
        assert!(start_polling_with(&remote, || panic!("cached-title worker"))
            .unwrap().is_none(), "cached titles keep no polling loop");
        let active = || RemoteWants { settings: true, ..Default::default() };
        let (first, _topic) = remote.observe(active()).unwrap();
        // Keep the initial host-list request pending without dispatching it, so
        // controlled iterations have active demand but spawn no backend tasks.
        assert!(matches!(&remote.plan(Instant::now())[..], [Request::Hosts]));
        let (worker, waits, resume) = controlled_polling(&remote);
        wait_for_poll(&waits);
        assert!(start_polling_with(&remote, || panic!("duplicate worker"))
            .unwrap().is_none(), "a second lease shares the existing worker");

        let (peer, _peer_topic) = remote.observe(active()).unwrap();
        drop(first);
        resume.send(()).unwrap();
        wait_for_poll(&waits);

        // Re-subscribe before the sleeping worker checks the now-idle set.
        drop(peer);
        let (second, _peer) = remote.observe(active()).unwrap();
        assert!(!remote.state.lock().unwrap().begin_polling());
        resume.send(()).unwrap();
        wait_for_poll(&waits);
        drop(second);
        resume.send(()).unwrap();
        drop(resume);
        worker.join().unwrap();
        assert!(waits.try_recv().is_err(), "idle retirement performs no further waits");
        assert!(!remote.state.lock().unwrap().polling);
        assert_eq!(remote.state.lock().unwrap().observers.len(), 1, "cached-title lease remains");
        assert!(!remote.state.lock().unwrap().begin_polling());

        // Re-subscribe after retirement: the new lease claims a fresh worker.
        let (third, _again) = remote.observe(active()).unwrap();
        let (worker, waits, resume) = controlled_polling(&remote);
        wait_for_poll(&waits);
        drop(third);
        resume.send(()).unwrap();
        drop(resume);
        worker.join().unwrap();
        assert!(waits.try_recv().is_err());
        assert!(!remote.state.lock().unwrap().polling);
        assert!(fake.calls.lock().unwrap().is_empty());
    }
    #[test]
    fn polling_workers_are_per_service_and_do_not_retain_idle_owners() {
        let (_first_directory, first, _) = fixture();
        let (_second_directory, second, _) = fixture();
        let active = || RemoteWants { settings: true, ..Default::default() };
        let (first_lease, _first_topic) = first.observe(active()).unwrap();
        let (second_lease, _second_topic) = second.observe(active()).unwrap();
        assert!(matches!(&first.plan(Instant::now())[..], [Request::Hosts]));
        assert!(matches!(&second.plan(Instant::now())[..], [Request::Hosts]));
        let (first_worker, first_waits, first_resume) = controlled_polling(&first);
        let (second_worker, second_waits, second_resume) = controlled_polling(&second);
        wait_for_poll(&first_waits);
        wait_for_poll(&second_waits);
        let first_weak = Arc::downgrade(&first);
        let second_weak = Arc::downgrade(&second);
        drop(first); drop(second); drop(first_lease); drop(second_lease);
        assert!(first_weak.upgrade().is_none());
        assert!(second_weak.upgrade().is_none());
        first_resume.send(()).unwrap(); second_resume.send(()).unwrap();
        drop(first_resume); drop(second_resume);
        first_worker.join().unwrap(); second_worker.join().unwrap();
        assert!(first_waits.try_recv().is_err());
        assert!(second_waits.try_recv().is_err());
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
                "somtus://pair?DI…  ({} characters)",
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
    fn open_tabs_are_titled_from_any_list_of_their_host_without_asking_it() {
        let (_directory, remote, fake) = fixture();
        answer(&remote, Request::Hosts);
        let orchestrator = format!("remote:{HOST}:0123456789abcdef");
        let unknown = format!("remote:{HOST}:fedcba9876543210");
        let (_observation, _topic) = remote
            .observe(RemoteWants {
                tabs: vec![orchestrator.clone(), unknown.clone()],
                ..Default::default()
            })
            .unwrap();
        // Titles are memory-only: an unseen window's tab asks the host nothing.
        assert!(remote.plan(Instant::now()).is_empty());
        let request = Request::Orchestrators { host: HOST.into() };
        remote.state.lock().unwrap().tree.apply(
            &request,
            Reply::Orchestrators(Ok(vec![tree::RemoteShell {
                id: "0123456789abcdef".into(),
                project_id: None,
                worktree_id: None,
                orchestrator: true,
                chat: false, chat_id: None,
                harness: Some("codex".into()),
                alive: true,
                cwd: "/FAKE".into(),
            }])),
        );
        let titles = remote.current().titles;
        assert_eq!(
            titles,
            vec![
                RemoteTabTitle {
                    key: orchestrator,
                    title: "⇄ Disposable Mac · orchestrator (codex) · 01234567".into(),
                },
                RemoteTabTitle {
                    key: unknown,
                    title: "⇄ Disposable Mac · fedcba98".into(),
                },
            ]
        );
        assert_eq!(fake.calls.lock().unwrap().len(), 1); // the registry only
    }

    #[test]
    fn an_unlisted_orchestrator_is_asked_for_once_and_explained() {
        let (_directory, remote, fake) = fixture();
        answer(&remote, Request::Hosts);
        let key = tree::project_key(HOST, PROJECT);
        // The fake fails the orchestrator list: the host cannot be reached.
        let result = remote.orchestrator(&key).unwrap();
        assert!(result.shell.is_none());
        assert_eq!(
            result.notice.as_deref(),
            Some("Cannot reach Disposable Mac")
        );
        let asked = |calls: &[String]| {
            calls
                .iter()
                .filter(|c| c.starts_with("Orchestrators"))
                .count()
        };
        assert_eq!(asked(&fake.calls.lock().unwrap()), 1);
        // Listed, but only a dead one and another project's: nothing to open.
        let request = Request::Orchestrators { host: HOST.into() };
        let orchestrator = |id: &str, project: Option<&str>, alive| tree::RemoteShell {
            id: id.into(),
            project_id: project.map(Into::into),
            worktree_id: None,
            orchestrator: true,
            chat: false, chat_id: None,
            harness: None,
            alive,
            cwd: "/FAKE".into(),
        };
        remote.state.lock().unwrap().tree.apply(
            &request,
            Reply::Orchestrators(Ok(vec![
                orchestrator("dead", Some(PROJECT), false),
                orchestrator("other", Some("other-project"), true),
            ])),
        );
        fake.orchestrators_listed.store(true, Ordering::SeqCst);
        assert_eq!(
            remote.orchestrator(&key).unwrap().notice.as_deref(),
            Some("Disposable Mac has no project orchestrator running")
        );
        // A live one already listed opens without asking again.
        remote.state.lock().unwrap().tree.apply(
            &request,
            Reply::Orchestrators(Ok(vec![orchestrator("live", Some(PROJECT), true)])),
        );
        let before = asked(&fake.calls.lock().unwrap());
        let found = remote.orchestrator(&key).unwrap();
        assert_eq!(found.shell.map(|shell| shell.id).as_deref(), Some("live"));
        assert_eq!(asked(&fake.calls.lock().unwrap()), before);
    }

    #[test]
    fn a_route_is_removed_only_by_a_good_registry_read_asked_once_per_interval() {
        let (_directory, remote, fake) = fixture();
        let reads = || {
            fake.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| *call == "Hosts")
                .count()
        };
        let every = tree::ROUTE_CHECK_INTERVAL;
        let second = Duration::from_secs(1);
        let now = Instant::now();
        // The first read finds the host, and the tree and its subscribers learn it too.
        assert!(!remote.route_removed_at(HOST, now));
        assert_eq!(reads(), 1);
        assert_eq!(remote.current().hosts.len(), 1);
        // It goes, and however many tabs ask, the read waits for its time.
        *fake.registry.lock().unwrap() = Some(Ok(Vec::new()));
        for _ in 0..20 {
            assert!(!remote.route_removed_at(HOST, now + second));
        }
        assert_eq!(reads(), 1);
        let later = now + every;
        assert!(remote.route_removed_at(HOST, later));
        assert_eq!(reads(), 2);
        assert!(remote.current().hosts.is_empty());
        // Known now, at no further cost.
        assert!(remote.route_removed_at(HOST, later + second));
        assert_eq!(reads(), 2);
        // A failed read neither clears that nor counts for anything else.
        *fake.registry.lock().unwrap() = Some(Err("no companion".into()));
        let again = later + every;
        assert!(remote.route_removed_at(HOST, again));
        assert_eq!(reads(), 3);
        // Paired again, which is noticed from this question alone, with no panel showing.
        *fake.registry.lock().unwrap() = None;
        assert!(remote.route_removed_at(HOST, again + second));
        assert!(!remote.route_removed_at(HOST, again + every));
        assert_eq!(remote.current().hosts.len(), 1);

        // A registry that never answered says nothing about anyone.
        let (_directory, remote, fake) = fixture();
        *fake.registry.lock().unwrap() = Some(Err("no companion".into()));
        assert!(!remote.route_removed_at(HOST, Instant::now()));
    }

    #[test]
    fn a_removed_hosts_names_and_launch_record_leave_the_file_and_its_window_stays_unpaired() {
        let (directory, remote, fake) = fixture();
        answer(&remote, Request::Hosts);
        answer(&remote, Request::Status { host: HOST.into() });
        answer(&remote, Request::Projects { host: HOST.into() });
        let key = tree::project_key(HOST, PROJECT);
        remote.select(Some(key.clone())).unwrap();
        let saved = remote_cache::load(directory.path());
        assert_eq!(saved.selected.as_deref(), Some(key.as_str()));
        assert!(saved.hosts.contains_key(HOST));
        assert_eq!(remote.launch_selection(false).as_deref(), Some(key.as_str()));

        *fake.registry.lock().unwrap() = Some(Ok(Vec::new()));
        answer(&remote, Request::Hosts);

        // Nothing else had to happen for the file to forget it.
        let saved = remote_cache::load(directory.path());
        assert_eq!(saved.selected, None);
        assert!(saved.hosts.is_empty());
        let file = std::fs::read_to_string(directory.path().join("remote-ui.json")).unwrap();
        assert!(!file.contains("Fixture project"), "{file}");
        // Neither this process nor the next one opens it again.
        assert_eq!(remote.launch_selection(false), None);
        let next = RemoteService::new(directory.path().into(), fake.clone());
        assert_eq!(next.launch_selection(false), None);
        // The window already on it stays there and shows why it is empty.
        let data = remote.current();
        assert_eq!(data.selected.as_deref(), Some(key.as_str()));
        assert!(data.projects.iter().any(|lists| lists.key == key && lists.unpaired));
        // A window that selects it anyway, as a restored one does, leaves no record either.
        remote.select(Some(key.clone())).unwrap();
        assert_eq!(remote_cache::load(directory.path()).selected, None);

        // Paired again under the same id, with that window still open: the record is back.
        *fake.registry.lock().unwrap() = None;
        answer(&remote, Request::Hosts);
        assert_eq!(
            remote_cache::load(directory.path()).selected.as_deref(),
            Some(key.as_str())
        );
        assert_eq!(remote.launch_selection(false).as_deref(), Some(key.as_str()));
    }

    #[test]
    fn the_launch_record_leaves_the_file_even_when_the_host_had_no_list_to_keep() {
        let (directory, remote, fake) = fixture();
        answer(&remote, Request::Hosts);
        let key = tree::project_key(HOST, PROJECT);
        remote.select(Some(key.clone())).unwrap();
        assert!(remote_cache::load(directory.path()).hosts.is_empty());
        *fake.registry.lock().unwrap() = Some(Ok(Vec::new()));
        answer(&remote, Request::Hosts);
        assert_eq!(remote_cache::load(directory.path()).selected, None);
        assert_eq!(remote.launch_selection(false), None);
    }

    #[test]
    fn a_failed_registry_read_changes_nothing_in_the_file() {
        let (directory, remote, fake) = fixture();
        answer(&remote, Request::Hosts);
        answer(&remote, Request::Status { host: HOST.into() });
        answer(&remote, Request::Projects { host: HOST.into() });
        let key = tree::project_key(HOST, PROJECT);
        remote.select(Some(key.clone())).unwrap();
        let before = std::fs::read(directory.path().join("remote-ui.json")).unwrap();
        *fake.registry.lock().unwrap() = Some(Err("no companion".into()));
        answer(&remote, Request::Hosts);
        assert_eq!(
            std::fs::read(directory.path().join("remote-ui.json")).unwrap(),
            before
        );
        assert_eq!(remote.launch_selection(false).as_deref(), Some(key.as_str()));
    }

    #[test]
    fn removing_a_host_forgets_its_sessions_at_once_and_a_failed_removal_does_not() {
        use riwork_engine::host_sessions::{Cache, Host as SessionHost, LocalSnapshot};
        let (directory, remote, fake) = fixture();
        let mut cache = Cache::default();
        for route in [HOST, "other-mac"] {
            cache.hosts.insert(
                route.into(),
                SessionHost::pending(Some(route.into()), route.into(), "relay"),
            );
            cache
                .apply_read(
                    route,
                    LocalSnapshot {
                        version: 1,
                        host_id: uuid::Uuid::new_v4().to_string(),
                        observed_at: 5,
                        capabilities: vec!["chats".into()],
                        total: 0,
                        partial: false,
                        sessions: Vec::new(),
                    },
                    5,
                )
                .unwrap();
        }
        cache.save(directory.path()).unwrap();
        let listed = |route: &str| Cache::open(directory.path()).hosts.contains_key(route);
        assert!(listed(HOST));

        fake.removal_fails.store(true, Ordering::SeqCst);
        assert!(remote.remove_host(HOST.into()).is_err());
        assert!(listed(HOST), "the route is still there, and so are its sessions");

        fake.removal_fails.store(false, Ordering::SeqCst);
        remote.remove_host(HOST.into()).unwrap();
        assert!(!listed(HOST));
        assert!(listed("other-mac"));
        // The registry is read again at the next plan, as before.
        let (_observation, _topic) = remote
            .observe(RemoteWants {
                settings: true,
                ..Default::default()
            })
            .unwrap();
        assert!(remote.plan(Instant::now()).contains(&Request::Hosts));
    }

    #[test]
    fn an_offline_hosts_reason_reaches_its_folder_note_and_status_text() {
        let (_directory, remote, fake) = fixture();
        answer(&remote, Request::Hosts);
        let reason = "SSH host identity changed; reconnect refused before sending requests";
        *fake.link.lock().unwrap() = Some(Ok(link_state(LinkState::Offline, Some(reason))));
        answer(&remote, Request::Status { host: HOST.into() });
        let data = remote.current();
        assert_eq!(data.hosts[0].link.as_deref(), Some("offline"));
        assert_eq!(data.hosts[0].status, reason);
        assert_eq!(data.folders[0].link.as_deref(), Some("offline"));
        assert_eq!(data.folders[0].note.as_deref(), Some(reason));

        let failed = "Cannot start the connection to this Mac: unreadable registry";
        *fake.link.lock().unwrap() = Some(Err(failed.into()));
        answer(&remote, Request::Status { host: HOST.into() });
        let data = remote.current();
        assert_eq!(data.hosts[0].status, failed);
        assert_eq!(data.folders[0].note.as_deref(), Some(failed));

        let plain = Link::Offline.text();
        *fake.link.lock().unwrap() = Some(Ok(link_state(LinkState::Offline, None)));
        answer(&remote, Request::Status { host: HOST.into() });
        let data = remote.current();
        assert_eq!(data.hosts[0].status, plain);
        assert_eq!(data.folders[0].note.as_deref(), Some(plain));

        *fake.link.lock().unwrap() = None;
        answer(&remote, Request::Status { host: HOST.into() });
        assert_eq!(remote.current().hosts[0].status, Link::Online.text());
    }

    #[test]
    fn a_creation_lost_with_the_link_may_exist_and_one_never_sent_is_refused() {
        let (_directory, remote, fake) = fixture();
        answer(&remote, Request::Hosts);
        answer(&remote, Request::Status { host: HOST.into() });
        answer(&remote, Request::Projects { host: HOST.into() });
        let key = tree::project_key(HOST, PROJECT);
        remote.select(Some(key.clone())).unwrap();
        for (error, prefix, may_exist) in [
            (RemoteError::Lost("the session ended".into()), "broken: ", true),
            // What a plain `offline` from an older daemon becomes.
            (RemoteError::Lost("host is offline: relay closed".into()), "broken: ", true),
            (RemoteError::Timeout, "broken: ", true),
            (
                RemoteError::Rpc {
                    code: "offline".into(),
                    message: "host is offline: relay closed".into(),
                },
                "refused: ",
                false,
            ),
            (
                RemoteError::NeedsSetup {
                    cause: "identity_changed".into(),
                    message: "Remove and add this host".into(),
                },
                "refused: ",
                false,
            ),
            (RemoteError::Unreachable("no daemon".into()), "refused: ", false),
        ] {
            let case = format!("{error:?}");
            let creates = fake.creates.load(Ordering::SeqCst);
            *fake.create_error.lock().unwrap() = Some(error);

            let refused = remote
                .create_project(HOST.into(), "New remote".into())
                .unwrap_err();
            assert!(refused.starts_with(prefix), "{case}: {refused}");
            let folder = remote.current().folders.remove(0);
            assert_eq!(
                folder.failure.as_deref().unwrap().contains("may exist"),
                may_exist,
                "{case}"
            );

            let refused = remote
                .create_shell(RemoteCreateShell {
                    project_key: key.clone(),
                    worktree_id: None,
                    kind: "shell".into(),
                    unrestricted: false,
                })
                .unwrap_err();
            assert!(refused.starts_with(prefix), "{case}: {refused}");
            let failure = remote
                .current()
                .projects
                .into_iter()
                .find(|lists| lists.key == key)
                .and_then(|lists| lists.failure)
                .unwrap();
            assert_eq!(failure.contains("may exist"), may_exist, "{case}: {failure}");
            // One request each, whatever came back.
            assert_eq!(fake.creates.load(Ordering::SeqCst), creates + 2, "{case}");
        }
    }

    #[test]
    fn a_plain_launch_reopens_the_remote_selection_and_a_folder_launch_clears_it() {
        let (directory, remote, fake) = fixture();
        let key = tree::project_key(HOST, PROJECT);
        remote.select(Some(key.clone())).unwrap();
        // A new process starts from what the last one kept.
        let next = RemoteService::new(directory.path().into(), fake.clone());
        assert_eq!(next.launch_selection(false).as_deref(), Some(key.as_str()));
        assert_eq!(next.launch_selection(true), None);
        assert_eq!(remote_cache::load(directory.path()).selected, None);
        let after = RemoteService::new(directory.path().into(), fake.clone());
        assert_eq!(after.launch_selection(false), None);
        // A damaged record never becomes a window.
        remote_cache::save(
            directory.path(),
            &Kept {
                selected: Some("remote:bad host:project".into()),
                hosts: Default::default(),
            },
        )
        .unwrap();
        let damaged = RemoteService::new(directory.path().into(), fake);
        assert_eq!(damaged.launch_selection(false), None);
    }

    #[test]
    fn both_pairing_schemes_are_accepted_and_hidden_in_errors() {
        for scheme in ["somtus", "riwork"] {
            let link = format!("{scheme}://pair?data=PRIVATE");
            assert_eq!(check_link(&link).unwrap(), link);
            assert_eq!(secret_error(&format!("bad {link}"), None), "bad [pairing link hidden]");
        }
    }

    #[test]
    fn invalid_pairing_and_project_names_never_reach_the_fake() {
        let (_directory, remote, fake) = fixture();
        for link in ["", "https://not-a-pair", "somtus://pair?with space"] {
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
        // The notice is the transport's message; the Shells panel keeps the
        // project's own failure row with the advice.
        assert_eq!(error, "broken: No answer in time");
        assert!(
            remote
                .current()
                .projects
                .iter()
                .find(|lists| lists.key == key)
                .and_then(|lists| lists.failure.clone())
                .unwrap()
                .contains("shell may exist")
        );
        assert!(
            fake.calls
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .contains("Project(\"fixture-project\"):shell:false") // a plain shell never is unrestricted
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
                .contains("Worktree(\"known-tree\"):codex:true") // this Mac's default setting
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
