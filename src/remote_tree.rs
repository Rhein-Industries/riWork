//! What the window knows about other Macs' projects, and when it asks.
//!
//! A paired Mac shows up in the Projects panel as a folder named after it, holding its
//! projects as ordinary rows. Selecting one of them makes it the window's project: the
//! Worktrees, Tasks and Shells panels then draw its lists. There is no GPUI and no I/O here.
//! `RemoteTree` is plain state that background tasks fill in, so the decisions (which list is
//! wanted now, which request may start, what a failed creation leaves behind) are tested
//! without a window or a daemon. `remote_service` runs the requests this module plans and
//! feeds the answers back.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{
    project_sort::{ProjectOrder, RemoteSortKey, sorted_remote_indices},
    remote_hosts::{Host, HostStatus, LinkState, RemoteError},
    store::TaskStatus,
};

/// How often a host's link state is asked for while someone is looking at it.
pub const STATUS_INTERVAL: Duration = Duration::from_secs(3);
/// How often a list the window is drawing is asked for again.
pub const LIST_INTERVAL: Duration = Duration::from_secs(5);
/// How often a host's project list is asked for again while its folder is open and shown.
pub const PROJECTS_INTERVAL: Duration = Duration::from_secs(10);
/// How often the figures on a project row (trees, tasks, live shells) are asked for again.
/// Each project costs three requests, so this is slower than the lists in front of the person.
pub const STATS_INTERVAL: Duration = Duration::from_secs(30);
/// How often the registry of hosts is read again while it is shown.
pub const HOSTS_INTERVAL: Duration = Duration::from_secs(10);
/// How many requests for project rows' figures may be out at once for one host.
const MAX_STATS_IN_FLIGHT: usize = 4;
/// A list reply travels through the relay and the host's CLI; this is generous.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// Starting an agent can take a minute on the host (docs/remote-protocol.md allows 90 s).
pub const CREATE_TIMEOUT: Duration = Duration::from_secs(90);

/// The longest project name the host accepts (Unicode scalar values).
const PROJECT_NAME_LIMIT: usize = 100;

/// Ids the window stores for something on another Mac start with this, so they can never be
/// found in, or collide with, the local store.
const KEY_PREFIX: &str = "remote:";

/// The id of a project on a host as the window stores it: `remote:{host}:{project}`.
pub fn project_key(host: &str, project: &str) -> String {
    format!("{KEY_PREFIX}{host}:{project}")
}

/// The host and project of a key made by [`project_key`]. Anything else, including the key of
/// a host's folder, is not one.
pub fn parse_project_key(key: &str) -> Option<(&str, &str)> {
    let (host, project) = key.strip_prefix(KEY_PREFIX)?.split_once(':')?;
    (!host.is_empty() && !project.is_empty()).then_some((host, project))
}

/// The id of a host's folder in the Projects panel, which is also what the set of collapsed
/// folders holds for it.
pub fn folder_key(host: &str) -> String {
    format!("{KEY_PREFIX}{host}")
}

/// The host of a key made by [`folder_key`].
pub fn parse_folder_key(key: &str) -> Option<&str> {
    let host = key.strip_prefix(KEY_PREFIX)?;
    (!host.is_empty() && !host.contains(':')).then_some(host)
}

/// When a request for one thing may start again.
#[derive(Clone, Debug, Default)]
struct Poll {
    in_flight: bool,
    asked_at: Option<Instant>,
}

impl Poll {
    /// `every` of `None` asks once and then only when invalidated.
    fn due(&self, now: Instant, every: Option<Duration>) -> bool {
        if self.in_flight {
            return false;
        }
        match (self.asked_at, every) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(at), Some(every)) => now.saturating_duration_since(at) >= every,
        }
    }

    fn begin(&mut self, now: Instant) {
        self.in_flight = true;
        self.asked_at = Some(now);
    }

    fn end(&mut self) {
        self.in_flight = false;
    }

    /// Make the next plan ask again, as after something changed on the host.
    fn invalidate(&mut self) {
        self.asked_at = None;
    }
}

/// A list fetched on demand. A failed refresh keeps the last good answer next to the error,
/// so rows do not vanish because one poll was lost.
#[derive(Clone, Debug)]
pub struct Fetch<T> {
    value: Option<T>,
    error: Option<String>,
    poll: Poll,
}

impl<T> Default for Fetch<T> {
    fn default() -> Self {
        Self {
            value: None,
            error: None,
            poll: Poll::default(),
        }
    }
}

impl<T: PartialEq> Fetch<T> {
    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Asked for and nothing to show yet.
    pub fn loading(&self) -> bool {
        self.value.is_none() && self.error.is_none()
    }

    /// Show `value` until the first answer comes, as when it was kept from an earlier run.
    fn seed(&mut self, value: T) {
        if self.value.is_none() {
            self.value = Some(value);
        }
    }

    /// Record an answer. Returns whether what is shown changed.
    fn finish(&mut self, result: Result<T, String>) -> bool {
        self.poll.end();
        match result {
            Ok(value) => {
                let changed = self.error.take().is_some() || self.value.as_ref() != Some(&value);
                self.value = Some(value);
                changed
            }
            Err(error) => {
                let changed = self.error.as_ref() != Some(&error);
                self.error = Some(error);
                changed
            }
        }
    }
}

/// The state of the host's link, for the dot beside its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Online,
    Offline,
}

impl Link {
    pub fn text(self) -> &'static str {
        match self {
            Self::Online => "Online",
            Self::Connecting => "Connecting…",
            // The daemon cannot tell a sleeping Mac from one that revoked this one.
            Self::Offline => "Offline (or access revoked)",
        }
    }
}

/// The strip above a remote tab while its host's link is down. Nothing while the link is
/// up, and nothing before the first answer, so a tab does not flash a warning on start.
pub fn link_strip(label: &str, link: Option<Link>) -> Option<String> {
    match link? {
        Link::Online => None,
        Link::Connecting => Some(format!("RECONNECTING · {label}")),
        Link::Offline => Some(format!(
            "RECONNECTING · {label} · offline, or access was revoked"
        )),
    }
}

/// The shell kinds `shell.create` accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewShellKind {
    Shell,
    Codex,
    Claude,
    Grok,
}

impl NewShellKind {
    /// Spelled exactly as the protocol wants it.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Grok => "grok",
        }
    }
}

/// Where `shell.create` starts a terminal: a project's root, or one of its worktrees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellScope {
    Project(String),
    Worktree(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteProject {
    pub id: String,
    pub name: String,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteWorktree {
    pub id: String,
    pub branch: String,
    pub path: String,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTask {
    pub id: String,
    pub title: String,
    pub details: String,
    pub status: TaskStatus,
    pub worktree_id: Option<String>,
}

/// A project shell or an orchestrator on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteShell {
    pub id: String,
    pub project_id: Option<String>,
    pub worktree_id: Option<String>,
    pub orchestrator: bool,
    /// An orchestrator that runs as a chat on its host (`"mode":"chat"`): it has no
    /// terminal to attach to, and its chat is followed on that Mac.
    pub chat: bool,
    pub harness: Option<String>,
    pub alive: bool,
    pub cwd: String,
}

impl RemoteShell {
    /// A short name for a row or a tab: what runs in it, and which one it is.
    pub fn display(&self) -> String {
        let what = match (self.orchestrator, self.harness.as_deref()) {
            (true, Some(harness)) if self.chat => format!("orchestrator chat ({harness})"),
            (true, Some(harness)) => format!("orchestrator ({harness})"),
            (true, None) => "orchestrator".to_owned(),
            (false, Some(harness)) => harness.to_owned(),
            (false, None) => "shell".to_owned(),
        };
        format!("{what} · {}", short_id(&self.id))
    }
}

/// The first eight characters of an id, which tell shells apart well enough to a person.
pub fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// The title of a remote tab. The prefix is what marks a tab as another Mac's.
pub fn remote_tab_title(host_label: &str, detail: &str) -> String {
    format!("⇄ {host_label} · {detail}")
}

/// Splits a remote tab's title into the host mark and the rest, to draw them apart.
pub fn split_remote_title(title: &str) -> Option<(&str, &str)> {
    let rest = title.strip_prefix("⇄ ")?;
    let end = rest.find(" · ")?;
    // The mark keeps the arrow and the separator so the two halves read as one title.
    let mark_len = "⇄ ".len() + end + " · ".len();
    Some(title.split_at(mark_len))
}

/// What a person typed as a project name, checked the way the host will check it so a
/// mistake is caught before a round trip. The host stays the authority: it checks again.
pub fn validate_project_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a project name.".to_owned());
    }
    if name.chars().count() > PROJECT_NAME_LIMIT {
        return Err(format!(
            "A project name can have at most {PROJECT_NAME_LIMIT} characters."
        ));
    }
    if name.len() > 255 {
        return Err("This name is too long for a folder name.".to_owned());
    }
    if name
        .chars()
        .any(|ch| ch.is_control() || ch == '\u{2028}' || ch == '\u{2029}')
    {
        return Err("A project name cannot contain control characters.".to_owned());
    }
    if name.contains(['/', '\\']) {
        return Err("A project name cannot contain / or \\.".to_owned());
    }
    if name.starts_with('.') {
        return Err("A project name cannot start with a dot.".to_owned());
    }
    if name.starts_with('-') {
        return Err("A project name cannot start with a dash.".to_owned());
    }
    Ok(name.to_owned())
}

/// What a creation request left behind. It is never retried by the window: the user decides.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Creation {
    #[default]
    Idle,
    Creating,
    Failed(String),
}

/// Which lists of one project to ask for, and how often.
#[derive(Clone, Copy, Debug, Default)]
struct Cadence {
    worktrees: Option<Duration>,
    tasks: Option<Duration>,
    shells: Option<Duration>,
}

impl Cadence {
    fn all(every: Duration) -> Self {
        Self {
            worktrees: Some(every),
            tasks: Some(every),
            shells: Some(every),
        }
    }
}

/// What is known about one project of a host.
#[derive(Debug, Default)]
struct ProjectData {
    worktrees: Fetch<Vec<RemoteWorktree>>,
    tasks: Fetch<Vec<RemoteTask>>,
    shells: Fetch<Vec<RemoteShell>>,
    /// The state of the last "new shell" asked of this project.
    create: Creation,
}

impl ProjectData {
    fn in_flight(&self) -> usize {
        usize::from(self.worktrees.poll.in_flight)
            + usize::from(self.tasks.poll.in_flight)
            + usize::from(self.shells.poll.in_flight)
    }

    /// Plan this project's due lists. `budget` limits how many may start; selected projects
    /// pass a budget that never runs out.
    fn plan(
        &mut self,
        host: &str,
        project: &str,
        cadence: Cadence,
        now: Instant,
        budget: &mut usize,
        out: &mut Vec<Request>,
    ) {
        let key = |host: &str, project: &str| (host.to_owned(), project.to_owned());
        if *budget > 0
            && cadence
                .worktrees
                .is_some_and(|every| self.worktrees.poll.due(now, Some(every)))
        {
            self.worktrees.poll.begin(now);
            *budget -= 1;
            let (host, project) = key(host, project);
            out.push(Request::Worktrees { host, project });
        }
        if *budget > 0
            && cadence
                .tasks
                .is_some_and(|every| self.tasks.poll.due(now, Some(every)))
        {
            self.tasks.poll.begin(now);
            *budget -= 1;
            let (host, project) = key(host, project);
            out.push(Request::Tasks { host, project });
        }
        if *budget > 0
            && cadence
                .shells
                .is_some_and(|every| self.shells.poll.due(now, Some(every)))
        {
            self.shells.poll.begin(now);
            *budget -= 1;
            let (host, project) = key(host, project);
            out.push(Request::Shells { host, project });
        }
    }

    fn invalidate(&mut self) {
        self.worktrees.poll.invalidate();
        self.tasks.poll.invalidate();
        self.shells.poll.invalidate();
    }
}

#[derive(Debug)]
struct HostNode {
    host: Host,
    /// `None` until the first answer; an error means the daemon could not be reached.
    status: Option<Result<HostStatus, String>>,
    status_poll: Poll,
    projects: Fetch<Vec<RemoteProject>>,
    orchestrators: Fetch<Vec<RemoteShell>>,
    data: BTreeMap<String, ProjectData>,
    /// The state of the last "new project" asked of this host.
    create: Creation,
}

impl HostNode {
    fn new(host: Host) -> Self {
        Self {
            host,
            status: None,
            status_poll: Poll::default(),
            projects: Fetch::default(),
            orchestrators: Fetch::default(),
            data: BTreeMap::new(),
            create: Creation::Idle,
        }
    }

    fn link(&self) -> Option<Link> {
        match self.status.as_ref()? {
            Ok(status) => Some(match status.state {
                LinkState::Online => Link::Online,
                LinkState::Connecting => Link::Connecting,
                LinkState::Offline => Link::Offline,
            }),
            Err(_) => Some(Link::Offline),
        }
    }

    /// Lists are only worth asking for once the link is known to be up.
    fn reachable(&self) -> bool {
        self.link() == Some(Link::Online)
    }

    fn invalidate_lists(&mut self) {
        self.projects.poll.invalidate();
        self.orchestrators.poll.invalidate();
        for data in self.data.values_mut() {
            data.invalidate();
        }
    }

    fn in_flight(&self) -> usize {
        usize::from(self.projects.poll.in_flight)
            + usize::from(self.orchestrators.poll.in_flight)
            + self
                .data
                .values()
                .map(ProjectData::in_flight)
                .sum::<usize>()
    }
}

/// One thing to ask a host or the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Hosts,
    Status { host: String },
    Projects { host: String },
    Orchestrators { host: String },
    Worktrees { host: String, project: String },
    Tasks { host: String, project: String },
    Shells { host: String, project: String },
}

/// The answer to a [`Request`], already parsed. The error is text for the window.
#[derive(Debug)]
pub enum Reply {
    Hosts(Result<Vec<Host>, String>),
    Status(Result<HostStatus, String>),
    Projects(Result<Vec<RemoteProject>, String>),
    Orchestrators(Result<Vec<RemoteShell>, String>),
    Worktrees(Result<Vec<RemoteWorktree>, String>),
    Tasks(Result<Vec<RemoteTask>, String>),
    Shells(Result<Vec<RemoteShell>, String>),
}

/// The selected remote project and which of its panels are on screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectedWants {
    pub host: String,
    pub project: String,
    pub worktrees: bool,
    pub tasks: bool,
    pub shells: bool,
}

/// What is on screen and so worth keeping up to date.
#[derive(Clone, Debug, Default)]
pub struct Wants {
    /// A Projects panel is showing the hosts' folders.
    pub folders: bool,
    /// The folders that are collapsed in it, by [`folder_key`].
    pub collapsed: HashSet<String>,
    /// A Settings panel listing the hosts is showing.
    pub settings: bool,
    /// Hosts that tabs are open on; their link state drives the reconnecting strip.
    pub tab_hosts: BTreeSet<String>,
    pub selected: Option<SelectedWants>,
}

/// The figures on a project row. A figure is unknown until its list has been read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub worktrees: Option<usize>,
    pub tasks_done: Option<usize>,
    pub tasks: Option<usize>,
    pub live: Option<usize>,
}

impl Stats {
    /// The line under a project's name, as a local project's: trees, tasks and live shells.
    pub fn line(&self) -> String {
        let count =
            |value: Option<usize>| value.map_or("–".to_owned(), |value| value.to_string());
        format!(
            "{} trees · {}/{} tasks · {} live",
            count(self.worktrees),
            count(self.tasks_done),
            count(self.tasks),
            count(self.live)
        )
    }
}

/// A project of a host as a row of its folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectView {
    /// The id the window stores for it, by [`project_key`].
    pub key: String,
    pub name: String,
    pub selected: bool,
    /// The host is off line: the row is what was listed last, and cannot be trusted.
    pub dimmed: bool,
    pub stats: Stats,
}

/// A host as a folder of the Projects panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderView {
    pub host: String,
    pub label: String,
    pub link: Option<Link>,
    pub collapsed: bool,
    /// A project is being created on the host.
    pub creating: bool,
    /// What a refused or lost project creation left, until dismissed.
    pub failure: Option<String>,
    /// A line under the heading: why there is nothing to list, or that the host is off line.
    pub note: Option<String>,
    /// How many projects the folder holds, whatever the search hides.
    pub count: usize,
    pub projects: Vec<ProjectView>,
}

/// What a click on a remote worktree does; see [`RemoteTree::click_worktree`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorktreeClick {
    /// Show the tab on this shell id.
    Tab(String),
    /// Open this live shell.
    Live(RemoteShell),
    /// Start a plain shell in the worktree.
    Start,
    /// The host's shells have not been read yet.
    Unknown,
}

/// A list the window draws, with what it has to say about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing<T> {
    pub items: Vec<T>,
    /// Asked for and nothing to show yet.
    pub loading: bool,
    pub error: Option<String>,
}

impl<T> Default for Listing<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            loading: false,
            error: None,
        }
    }
}

impl<T: PartialEq + Clone> Listing<T> {
    fn of(fetch: &Fetch<Vec<T>>) -> Self {
        Self {
            items: fetch.value().cloned().unwrap_or_default(),
            loading: fetch.loading(),
            error: fetch.error().map(str::to_owned),
        }
    }
}

/// What the Worktrees, Tasks and Shells panels draw for the selected remote project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedView {
    pub host: String,
    pub project: String,
    pub host_label: String,
    pub project_name: String,
    pub link: Option<Link>,
    /// The registry has been read and does not list the host any more.
    pub unpaired: bool,
    pub worktrees: Listing<RemoteWorktree>,
    pub tasks: Listing<RemoteTask>,
    /// The project's orchestrators first, then the host's global ones, then its shells.
    pub shells: Listing<RemoteShell>,
    pub creating: bool,
    pub failure: Option<String>,
}

#[derive(Debug, Default)]
pub struct RemoteTree {
    hosts: Vec<HostNode>,
    hosts_poll: Poll,
    hosts_error: Option<String>,
    /// The registry has been read at least once, so a host missing from it is gone.
    hosts_known: bool,
    /// Project lists kept from an earlier run, for hosts the registry has not named yet.
    kept: BTreeMap<String, Vec<RemoteProject>>,
    /// The project lists changed since they were last handed out to be kept.
    kept_dirty: bool,
    /// Shells this window had the host make, by host and id, until the host's lists name them.
    /// A tab opened for one is then known by the worktree it was started in at once.
    launched: BTreeMap<(String, String), RemoteShell>,
}

impl RemoteTree {
    pub fn hosts_error(&self) -> Option<&str> {
        self.hosts_error.as_deref()
    }

    pub fn hosts(&self) -> impl Iterator<Item = (&Host, Option<Link>)> {
        self.hosts.iter().map(|node| (&node.host, node.link()))
    }

    pub fn host_label(&self, id: &str) -> Option<&str> {
        self.node(id).map(|node| node.host.label.as_str())
    }

    /// The host's name, or the start of its id when the registry does not have it.
    pub fn host_name(&self, id: &str) -> String {
        self.host_label(id)
            .unwrap_or_else(|| short_id(id))
            .to_owned()
    }

    /// The project's name as the host's folder lists it, or the start of its id. Before the
    /// registry has named the host this is what the last run kept.
    pub fn project_name(&self, host: &str, project: &str) -> String {
        let listed = match self.node(host) {
            Some(node) => node.projects.value(),
            None => self.kept.get(host),
        };
        listed
            .and_then(|projects| projects.iter().find(|candidate| candidate.id == project))
            .map_or_else(|| short_id(project).to_owned(), |found| found.name.clone())
    }

    /// The strip a tab on `host` shows, if any.
    pub fn strip_for(&self, host: &str) -> Option<String> {
        let node = self.node(host)?;
        link_strip(&node.host.label, node.link())
    }

    /// The shell as the host last listed it, if a list this window loaded has it, or else as
    /// the host answered when it was asked to make it.
    pub fn known_shell(&self, host: &str, shell_id: &str) -> Option<&RemoteShell> {
        let listed = self.node(host).and_then(|node| {
            node.orchestrators
                .value()
                .into_iter()
                .flatten()
                .chain(
                    node.data
                        .values()
                        .filter_map(|data| data.shells.value())
                        .flatten(),
                )
                .find(|shell| shell.id == shell_id)
        });
        listed.or_else(|| self.launched.get(&(host.to_owned(), shell_id.to_owned())))
    }

    /// The title for a tab on `shell_id` of `host`: the shell's kind when a list knows it,
    /// otherwise its short id.
    pub fn tab_title(&self, host: &str, shell_id: &str) -> String {
        let detail = self
            .known_shell(host, shell_id)
            .map(RemoteShell::display)
            .unwrap_or_else(|| short_id(shell_id).to_owned());
        remote_tab_title(&self.host_name(host), &detail)
    }

    /// The host's orchestrator of `project` (or its global one when `project` is `None`), as
    /// last listed. Only a live one can be opened.
    pub fn orchestrator(&self, host: &str, project: Option<&str>) -> Option<&RemoteShell> {
        self.node(host)?
            .orchestrators
            .value()?
            .iter()
            .find(|shell| shell.alive && shell.project_id.as_deref() == project)
    }

    /// The live shells of a project as last listed: what the status bar counts.
    pub fn live_shells(&self, host: &str, project: &str) -> Option<usize> {
        let shells = self.node(host)?.data.get(project)?.shells.value()?;
        Some(shells.iter().filter(|shell| shell.alive).count())
    }

    /// The branch of a worktree of the project, as last listed.
    pub fn worktree_branch(&self, host: &str, project: &str, worktree: &str) -> Option<&str> {
        self.node(host)?
            .data
            .get(project)?
            .worktrees
            .value()?
            .iter()
            .find(|candidate| candidate.id == worktree)
            .map(|found| found.branch.as_str())
    }

    fn node(&self, id: &str) -> Option<&HostNode> {
        self.hosts.iter().find(|node| node.host.id == id)
    }

    fn node_mut(&mut self, id: &str) -> Option<&mut HostNode> {
        self.hosts.iter_mut().find(|node| node.host.id == id)
    }

    /// Replace the registry's hosts. Hosts that stay keep what was loaded for them, and a new
    /// one starts from the project list kept from an earlier run. Returns whether anything
    /// changed.
    pub fn set_hosts(&mut self, hosts: Vec<Host>) -> bool {
        self.hosts_known = true;
        let before = self
            .hosts
            .iter()
            .map(|node| node.host.clone())
            .collect::<Vec<_>>();
        if before == hosts {
            return false;
        }
        let mut previous = std::mem::take(&mut self.hosts);
        self.hosts = hosts
            .into_iter()
            .map(
                |host| match previous.iter().position(|node| node.host.id == host.id) {
                    Some(index) => {
                        let mut node = previous.swap_remove(index);
                        node.host = host;
                        node
                    }
                    None => {
                        let mut node = HostNode::new(host);
                        if let Some(projects) = self.kept.get(&node.host.id) {
                            node.projects.seed(projects.clone());
                        }
                        node
                    }
                },
            )
            .collect();
        true
    }

    /// Project lists kept from an earlier run: until a host answers, its folder lists these,
    /// dimmed if it cannot be reached.
    pub fn keep(&mut self, kept: BTreeMap<String, Vec<RemoteProject>>) {
        for node in &mut self.hosts {
            if let Some(projects) = kept.get(&node.host.id) {
                node.projects.seed(projects.clone());
            }
        }
        self.kept = kept;
    }

    /// Whether the project lists changed since they were last asked for to be kept.
    pub fn take_kept_dirty(&mut self) -> bool {
        std::mem::take(&mut self.kept_dirty)
    }

    /// The project lists to keep for the next run: what each paired host last listed, and for
    /// a host that has not answered yet what was kept before. A host that is no longer paired
    /// is forgotten once the registry has been read.
    pub fn kept(&mut self) -> BTreeMap<String, Vec<RemoteProject>> {
        let mut kept = std::mem::take(&mut self.kept);
        if self.hosts_known {
            kept.retain(|id, _| self.node(id).is_some());
        }
        for node in &self.hosts {
            if let Some(projects) = node.projects.value() {
                kept.insert(node.host.id.clone(), projects.clone());
            }
        }
        self.kept = kept.clone();
        kept
    }

    /// Read the registry again at the next plan, as after a host was added or removed.
    pub fn invalidate_hosts(&mut self) {
        self.hosts_poll.invalidate();
    }

    /// Decide what to ask for now, and mark those requests as running. Nothing is planned
    /// for something nobody is looking at: a hidden panel or a collapsed folder costs no
    /// traffic. Creations are never planned here: only a person starts one.
    pub fn plan(&mut self, now: Instant, wants: &Wants) -> Vec<Request> {
        let mut requests = Vec::new();
        let looking = wants.folders || wants.settings;
        let wanted = looking || !wants.tab_hosts.is_empty() || wants.selected.is_some();
        if wanted {
            // Tabs and a selection alone need the labels once; a shown list is kept current.
            let every = looking.then_some(HOSTS_INTERVAL);
            if self.hosts_poll.due(now, every) {
                self.hosts_poll.begin(now);
                requests.push(Request::Hosts);
            }
        }
        for node in &mut self.hosts {
            let host = node.host.id.clone();
            let selected = wants.selected.as_ref().filter(|chosen| chosen.host == host);
            let watched = looking || wants.tab_hosts.contains(&host) || selected.is_some();
            if watched && node.status_poll.due(now, Some(STATUS_INTERVAL)) {
                node.status_poll.begin(now);
                requests.push(Request::Status { host: host.clone() });
            }
            if !node.reachable() {
                continue;
            }
            let folder_open = wants.folders && !wants.collapsed.contains(&folder_key(&host));
            if folder_open && node.projects.poll.due(now, Some(PROJECTS_INTERVAL)) {
                node.projects.poll.begin(now);
                requests.push(Request::Projects { host: host.clone() });
            }
            if let Some(chosen) = selected {
                let cadence = |wanted: bool| wanted.then_some(LIST_INTERVAL);
                let cadence = Cadence {
                    worktrees: cadence(chosen.worktrees),
                    tasks: cadence(chosen.tasks),
                    shells: cadence(chosen.shells),
                };
                // What the person is looking at is never held back by the limit on the rows' figures.
                let mut unlimited = usize::MAX;
                node.data.entry(chosen.project.clone()).or_default().plan(
                    &host,
                    &chosen.project,
                    cadence,
                    now,
                    &mut unlimited,
                    &mut requests,
                );
                if chosen.shells && node.orchestrators.poll.due(now, Some(LIST_INTERVAL)) {
                    node.orchestrators.poll.begin(now);
                    requests.push(Request::Orchestrators { host: host.clone() });
                }
            }
            if folder_open {
                // The figures on the rows: a few at a time, so a host with many projects is
                // not flooded.
                let mut budget = MAX_STATS_IN_FLIGHT.saturating_sub(node.in_flight());
                let ids = node
                    .projects
                    .value()
                    .map(|projects| projects.iter().map(|project| project.id.clone()).collect())
                    .unwrap_or_else(Vec::<String>::new);
                for id in ids {
                    if budget == 0 {
                        break;
                    }
                    node.data.entry(id.clone()).or_default().plan(
                        &host,
                        &id,
                        Cadence::all(STATS_INTERVAL),
                        now,
                        &mut budget,
                        &mut requests,
                    );
                }
            }
        }
        requests
    }

    /// Take an answer in. Returns whether what is drawn changed.
    pub fn apply(&mut self, request: &Request, reply: Reply) -> bool {
        match (request, reply) {
            (Request::Hosts, Reply::Hosts(result)) => {
                self.hosts_poll.end();
                match result {
                    Ok(hosts) => {
                        let error = self.hosts_error.take().is_some();
                        self.set_hosts(hosts) || error
                    }
                    Err(error) => {
                        let changed = self.hosts_error.as_ref() != Some(&error);
                        self.hosts_error = Some(error);
                        changed
                    }
                }
            }
            (Request::Status { host }, Reply::Status(result)) => {
                let Some(node) = self.node_mut(host) else {
                    return false;
                };
                node.status_poll.end();
                let was = node.link();
                node.status = Some(result);
                if was != Some(Link::Online) && node.link() == Some(Link::Online) {
                    // Back on line: what was listed may be stale, so ask again at once.
                    node.invalidate_lists();
                }
                // The round-trip time moves on every poll and is not drawn.
                was != node.link()
            }
            (Request::Projects { host }, Reply::Projects(result)) => {
                let Some(node) = self.node_mut(host) else {
                    return false;
                };
                let changed = node.projects.finish(result);
                // Only a changed answer is worth writing down for the next run.
                self.kept_dirty |= changed && node.projects.value().is_some();
                changed
            }
            (Request::Orchestrators { host }, Reply::Orchestrators(result)) => self
                .node_mut(host)
                .is_some_and(|node| node.orchestrators.finish(result)),
            (Request::Worktrees { host, project }, Reply::Worktrees(result)) => self
                .data_mut(host, project)
                .is_some_and(|data| data.worktrees.finish(result)),
            (Request::Tasks { host, project }, Reply::Tasks(result)) => self
                .data_mut(host, project)
                .is_some_and(|data| data.tasks.finish(result)),
            (Request::Shells { host, project }, Reply::Shells(result)) => self
                .data_mut(host, project)
                .is_some_and(|data| data.shells.finish(result)),
            // An answer for something else is a bug in the caller; ignoring it is safest.
            _ => false,
        }
    }

    fn data_mut(&mut self, host: &str, project: &str) -> Option<&mut ProjectData> {
        self.node_mut(host)?.data.get_mut(project)
    }

    /// The user asked for a new shell in a project. Returns whether the request may be sent:
    /// not while another creation for that project is still out, and not for a host this
    /// window does not know. Sending it is the caller's job, exactly once.
    pub fn begin_shell(&mut self, host: &str, project: &str) -> bool {
        let Some(node) = self.node_mut(host) else {
            return false;
        };
        let data = node.data.entry(project.to_owned()).or_default();
        if data.create == Creation::Creating {
            return false;
        }
        data.create = Creation::Creating;
        true
    }

    /// Record how a shell creation ended. On success the new shell is returned so it can be
    /// opened. After an answer that may not have been the whole story (no reply in time, an
    /// unreadable one) the list is refreshed so the person can see whether the shell exists,
    /// and nothing is retried.
    pub fn finish_shell(
        &mut self,
        host: &str,
        project: &str,
        result: Result<RemoteShell, RemoteError>,
    ) -> Option<RemoteShell> {
        let label = self.host_name(host);
        let data = self.data_mut(host, project)?;
        match result {
            Ok(shell) => {
                data.create = Creation::Idle;
                data.shells.poll.invalidate();
                self.launched
                    .insert((host.to_owned(), shell.id.clone()), shell.clone());
                Some(shell)
            }
            Err(error) => {
                data.create = Creation::Failed(creation_failure(&error, &label, "shell"));
                if outcome_unknown(&error) {
                    data.shells.poll.invalidate();
                }
                None
            }
        }
    }

    /// What a click on a worktree of a project does. This is the local rule of
    /// `Workspace::select_worktree`, applied to the host's shells in the same order:
    ///
    /// 1. a tab is already open on a shell started in that worktree: show it (whether or not the
    ///    shell is still alive, as a local tab is);
    /// 2. otherwise the project's newest live shell of that worktree, of any kind (agents and
    ///    orchestrators count as they do locally);
    /// 3. otherwise a plain shell is started in the worktree, as `Workspace::add_tab` does.
    ///
    /// `open_tabs` are the ids of the shells of `host` that the window has tabs on.
    pub fn click_worktree(
        &self,
        host: &str,
        project: &str,
        worktree: &str,
        open_tabs: &[&str],
    ) -> WorktreeClick {
        let in_worktree = |shell: &RemoteShell| shell.worktree_id.as_deref() == Some(worktree);
        if let Some(open) = open_tabs
            .iter()
            .find(|id| self.known_shell(host, id).is_some_and(in_worktree))
        {
            return WorktreeClick::Tab((*open).to_owned());
        }
        // Without the host's list, "no live shell" is unknown: starting one then could make a
        // second one beside a shell nobody has heard of yet.
        let Some(shells) = self
            .node(host)
            .and_then(|node| node.data.get(project))
            .and_then(|data| data.shells.value())
        else {
            return WorktreeClick::Unknown;
        };
        match shells.iter().rev().find(|shell| {
            shell.alive && shell.project_id.as_deref() == Some(project) && in_worktree(shell)
        }) {
            Some(live) => WorktreeClick::Live(live.clone()),
            None => WorktreeClick::Start,
        }
    }

    /// The user asked for a new project on `host`; the same rules as `begin_shell`.
    pub fn begin_project(&mut self, host: &str) -> bool {
        let Some(node) = self.node_mut(host) else {
            return false;
        };
        if node.create == Creation::Creating {
            return false;
        }
        node.create = Creation::Creating;
        true
    }

    /// Record how a project creation ended; the new project is returned on success and its
    /// folder asks for its list at once.
    pub fn finish_project(
        &mut self,
        host: &str,
        result: Result<RemoteProject, RemoteError>,
    ) -> Option<RemoteProject> {
        let label = self.host_name(host);
        let node = self.node_mut(host)?;
        match result {
            Ok(project) => {
                node.create = Creation::Idle;
                node.projects.poll.invalidate();
                Some(project)
            }
            Err(error) => {
                node.create = Creation::Failed(creation_failure(&error, &label, "project"));
                if outcome_unknown(&error) {
                    node.projects.poll.invalidate();
                }
                None
            }
        }
    }

    /// Dismiss what a failed shell creation left on a project.
    pub fn dismiss_shell_failure(&mut self, host: &str, project: &str) {
        if let Some(data) = self.data_mut(host, project)
            && matches!(data.create, Creation::Failed(_))
        {
            data.create = Creation::Idle;
        }
    }

    /// Dismiss what a failed project creation left on a host's folder.
    pub fn dismiss_project_failure(&mut self, host: &str) {
        if let Some(node) = self.node_mut(host)
            && matches!(node.create, Creation::Failed(_))
        {
            node.create = Creation::Idle;
        }
    }

    /// The folders to draw for `query` (already lowercased and trimmed; empty shows
    /// everything), in the registry's order. Projects are ordered as `order` says, as local
    /// ones are, and a folder collapsed in `collapsed` hides them unless a search is on.
    pub fn folders(
        &self,
        query: &str,
        order: ProjectOrder,
        collapsed: &HashSet<String>,
        selected: Option<&str>,
    ) -> Vec<FolderView> {
        self.hosts
            .iter()
            .filter_map(|node| node.folder(query, order, collapsed, selected))
            .collect()
    }

    /// What the panels draw for the project under `key`. `None` when `key` is not a remote
    /// project's.
    pub fn selected_view(&self, key: &str) -> Option<SelectedView> {
        let (host, project) = parse_project_key(key)?;
        let Some(node) = self.node(host) else {
            return Some(SelectedView {
                host: host.to_owned(),
                project: project.to_owned(),
                host_label: short_id(host).to_owned(),
                project_name: short_id(project).to_owned(),
                link: None,
                unpaired: self.hosts_known,
                worktrees: Listing::default(),
                tasks: Listing::default(),
                shells: Listing::default(),
                creating: false,
                failure: None,
            });
        };
        let empty = ProjectData::default();
        let data = node.data.get(project).unwrap_or(&empty);
        let mut shells = Listing::of(&data.shells);
        // The orchestrators come first: the project's own, then the host's global ones.
        let orchestrators = node.orchestrators.value().map(Vec::as_slice).unwrap_or(&[]);
        let own = orchestrators
            .iter()
            .filter(|shell| shell.project_id.as_deref() == Some(project));
        let global = orchestrators
            .iter()
            .filter(|shell| shell.project_id.is_none());
        shells.items = own
            .chain(global)
            .cloned()
            .chain(std::mem::take(&mut shells.items))
            .collect();
        Some(SelectedView {
            host: host.to_owned(),
            project: project.to_owned(),
            host_label: node.host.label.clone(),
            project_name: self.project_name(host, project),
            link: node.link(),
            unpaired: false,
            worktrees: Listing::of(&data.worktrees),
            tasks: Listing::of(&data.tasks),
            shells,
            creating: data.create == Creation::Creating,
            failure: match &data.create {
                Creation::Failed(message) => Some(message.clone()),
                _ => None,
            },
        })
    }
}

impl HostNode {
    fn folder(
        &self,
        query: &str,
        order: ProjectOrder,
        collapsed: &HashSet<String>,
        selected: Option<&str>,
    ) -> Option<FolderView> {
        let searching = !query.is_empty();
        let host_matches = !searching || self.host.label.to_lowercase().contains(query);
        let listed = self.projects.value().map(Vec::as_slice).unwrap_or_default();
        let link = self.link();
        let dimmed = link == Some(Link::Offline);
        let keys = listed
            .iter()
            .map(|project| {
                let live = self
                    .data
                    .get(&project.id)
                    .and_then(|data| data.shells.value())
                    .map(|shells| shells.iter().filter(|shell| shell.alive).count() as u64);
                RemoteSortKey {
                    name: &project.name,
                    id: &project.id,
                    created_at: project.created_at,
                    live,
                }
            })
            .collect::<Vec<_>>();
        let projects = sorted_remote_indices(&keys, order)
            .into_iter()
            .map(|index| &listed[index])
            .filter(|project| {
                host_matches
                    || project.name.to_lowercase().contains(query)
                    || project.id.to_lowercase().contains(query)
            })
            .map(|project| {
                let key = project_key(&self.host.id, &project.id);
                ProjectView {
                    selected: selected == Some(key.as_str()),
                    key,
                    name: project.name.clone(),
                    dimmed,
                    stats: self.stats(&project.id),
                }
            })
            .collect::<Vec<_>>();
        if searching && !host_matches && projects.is_empty() {
            return None;
        }
        let note = match (link, self.projects.value(), self.projects.error()) {
            (Some(Link::Offline), ..) => Some(Link::Offline.text().to_owned()),
            (_, None, Some(error)) => Some(error.to_owned()),
            (Some(Link::Connecting) | None, None, None) => Some(Link::Connecting.text().to_owned()),
            (_, None, None) => Some("Loading projects…".to_owned()),
            (_, Some(listed), error) => match error {
                Some(error) => Some(error.to_owned()),
                None if listed.is_empty() && !searching => Some("No projects".to_owned()),
                None => None,
            },
        };
        Some(FolderView {
            host: self.host.id.clone(),
            label: self.host.label.clone(),
            link,
            collapsed: !searching && collapsed.contains(&folder_key(&self.host.id)),
            creating: self.create == Creation::Creating,
            failure: match &self.create {
                Creation::Failed(message) => Some(message.clone()),
                _ => None,
            },
            note,
            count: listed.len(),
            projects,
        })
    }

    fn stats(&self, project: &str) -> Stats {
        let Some(data) = self.data.get(project) else {
            return Stats::default();
        };
        let tasks = data.tasks.value();
        Stats {
            worktrees: data.worktrees.value().map(Vec::len),
            tasks: tasks.map(Vec::len),
            tasks_done: tasks.map(|tasks| {
                tasks
                    .iter()
                    .filter(|task| task.status == TaskStatus::Done)
                    .count()
            }),
            live: data
                .shells
                .value()
                .map(|shells| shells.iter().filter(|shell| shell.alive).count()),
        }
    }
}

/// Whether a failed request may still have run on the host. A refusal is definite, and so
/// is a daemon that could not be reached: nothing was sent. A reply that never came, or that
/// could not be read, is not.
pub fn outcome_unknown(error: &RemoteError) -> bool {
    matches!(error, RemoteError::Timeout | RemoteError::Protocol(_))
}

fn creation_failure(error: &RemoteError, host: &str, what: &str) -> String {
    if outcome_unknown(error) {
        // Never retried for the person: the first request may be running or done.
        format!(
            "No answer from {host}, so the {what} may exist. Check the list before trying again."
        )
    } else if error.is_unsupported() {
        format!("{host} runs a RiWork that cannot create a {what} remotely. Update it.")
    } else {
        error.message()
    }
}

/// Parameters of `shell.create`. Only an agent can be unrestricted; the protocol refuses
/// the flag on a plain shell, so it is left out.
pub fn shell_create_params(scope: &ShellScope, kind: NewShellKind, unrestricted: bool) -> Value {
    let mut params = match scope {
        ShellScope::Project(id) => json!({"project_id": id}),
        ShellScope::Worktree(id) => json!({"worktree_id": id}),
    };
    params["kind"] = json!(kind.wire());
    if unrestricted && kind != NewShellKind::Shell {
        params["unrestricted"] = json!(true);
    }
    params
}

/// Parameters of `project.create`; the name was validated by [`validate_project_name`].
pub fn project_create_params(name: &str) -> Value {
    json!({"name": name})
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

/// The array under `key`, parsed entry by entry. Entries that do not parse are skipped:
/// clients tolerate additive and odd fields, and one bad entry must not hide the rest.
fn list<T>(
    value: &Value,
    key: &str,
    parse: impl Fn(&Value) -> Option<T>,
) -> Result<Vec<T>, String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(parse).collect())
        .ok_or_else(|| format!("The host's reply has no \"{key}\" list."))
}

fn project(value: &Value) -> Option<RemoteProject> {
    Some(RemoteProject {
        id: text(value, "id")?,
        name: text(value, "name").unwrap_or_default(),
        created_at: value.get("created_at").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn worktree(value: &Value) -> Option<RemoteWorktree> {
    Some(RemoteWorktree {
        id: text(value, "id")?,
        branch: text(value, "branch").unwrap_or_default(),
        path: text(value, "path").unwrap_or_default(),
        primary: value
            .get("is_primary")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn task(value: &Value) -> Option<RemoteTask> {
    Some(RemoteTask {
        id: text(value, "id")?,
        title: text(value, "title").unwrap_or_default(),
        details: text(value, "details").unwrap_or_default(),
        status: text(value, "status")
            .and_then(|status| TaskStatus::parse(&status).ok())
            .unwrap_or_default(),
        worktree_id: text(value, "worktree_id"),
    })
}

fn session(value: &Value) -> Option<RemoteShell> {
    Some(RemoteShell {
        id: text(value, "id")?,
        project_id: text(value, "project_id"),
        worktree_id: text(value, "worktree_id"),
        orchestrator: text(value, "kind").as_deref() == Some("orchestrator"),
        chat: text(value, "mode").as_deref() == Some("chat"),
        harness: text(value, "harness"),
        alive: value.get("alive").and_then(Value::as_bool).unwrap_or(false),
        cwd: text(value, "cwd").unwrap_or_default(),
    })
}

pub fn parse_projects(value: &Value) -> Result<Vec<RemoteProject>, String> {
    list(value, "projects", project)
}

pub fn parse_worktrees(value: &Value) -> Result<Vec<RemoteWorktree>, String> {
    list(value, "worktrees", worktree)
}

pub fn parse_tasks(value: &Value) -> Result<Vec<RemoteTask>, String> {
    list(value, "tasks", task)
}

pub fn parse_shells(value: &Value) -> Result<Vec<RemoteShell>, String> {
    list(value, "shells", session)
}

pub fn parse_orchestrators(value: &Value) -> Result<Vec<RemoteShell>, String> {
    list(value, "orchestrators", session)
}

/// The shell `shell.create` made.
pub fn parse_created_shell(value: &Value) -> Result<RemoteShell, String> {
    value
        .get("shell")
        .and_then(session)
        .ok_or_else(|| "The host did not describe the shell it created.".to_owned())
}

/// The project `project.create` made.
pub fn parse_created_project(value: &Value) -> Result<RemoteProject, String> {
    value
        .get("project")
        .and_then(project)
        .ok_or_else(|| "The host did not describe the project it created.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_sort::ProjectSort;

    fn host(id: &str, label: &str) -> Host {
        Host {
            id: id.to_owned(),
            label: label.to_owned(),
        }
    }

    fn status(state: LinkState) -> HostStatus {
        HostStatus {
            state,
            rtt_ms: None,
            since: None,
            reason: None,
            label: None,
        }
    }

    fn project(id: &str, name: &str, created_at: u64) -> RemoteProject {
        RemoteProject {
            id: id.to_owned(),
            name: name.to_owned(),
            created_at,
        }
    }

    fn shell(id: &str, project: Option<&str>, worktree: Option<&str>) -> RemoteShell {
        RemoteShell {
            id: id.to_owned(),
            project_id: project.map(str::to_owned),
            worktree_id: worktree.map(str::to_owned),
            orchestrator: false,
            chat: false,
            harness: None,
            alive: true,
            cwd: "/Users/me/app".to_owned(),
        }
    }

    fn task(id: &str, status: TaskStatus, worktree: Option<&str>) -> RemoteTask {
        RemoteTask {
            id: id.to_owned(),
            title: format!("task {id}"),
            details: String::new(),
            status,
            worktree_id: worktree.map(str::to_owned),
        }
    }

    fn rpc(code: &str, message: &str) -> RemoteError {
        RemoteError::Rpc {
            code: code.to_owned(),
            message: message.to_owned(),
        }
    }

    fn by_name() -> ProjectOrder {
        ProjectOrder::for_sort(ProjectSort::Name)
    }

    fn folders(tree: &RemoteTree, query: &str) -> Vec<FolderView> {
        tree.folders(query, by_name(), &HashSet::new(), None)
    }

    fn names(folder: &FolderView) -> Vec<&str> {
        folder
            .projects
            .iter()
            .map(|project| project.name.as_str())
            .collect()
    }

    fn set_link(tree: &mut RemoteTree, state: LinkState) {
        tree.apply(
            &Request::Status { host: "h1".into() },
            Reply::Status(Ok(status(state))),
        );
    }

    /// An online host called "Studio" that listed two projects.
    fn studio() -> RemoteTree {
        let mut tree = RemoteTree::default();
        tree.set_hosts(vec![host("h1", "Studio")]);
        set_link(&mut tree, LinkState::Online);
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(vec![project("p1", "app", 20), project("p2", "Web", 10)])),
        );
        tree
    }

    fn wants_folders() -> Wants {
        Wants {
            folders: true,
            ..Wants::default()
        }
    }

    fn wants_selected(panels: &str) -> Wants {
        Wants {
            selected: Some(SelectedWants {
                host: "h1".into(),
                project: "p1".into(),
                worktrees: panels.contains('w'),
                tasks: panels.contains('t'),
                shells: panels.contains('s'),
            }),
            ..Wants::default()
        }
    }

    /// What a tick asks for, leaving out the one-off registry read and the link.
    fn lists(tree: &mut RemoteTree, now: Instant, wants: &Wants) -> Vec<Request> {
        tree.plan(now, wants)
            .into_iter()
            .filter(|request| !matches!(request, Request::Hosts | Request::Status { .. }))
            .collect()
    }

    fn answer_all(tree: &mut RemoteTree, requests: &[Request]) {
        for request in requests {
            let reply = match request {
                Request::Projects { .. } => Reply::Projects(Ok(vec![])),
                Request::Orchestrators { .. } => Reply::Orchestrators(Ok(vec![])),
                Request::Worktrees { .. } => Reply::Worktrees(Ok(vec![])),
                Request::Tasks { .. } => Reply::Tasks(Ok(vec![])),
                Request::Shells { .. } => Reply::Shells(Ok(vec![])),
                Request::Hosts => Reply::Hosts(Ok(vec![host("h1", "Studio")])),
                Request::Status { .. } => Reply::Status(Ok(status(LinkState::Online))),
            };
            tree.apply(request, reply);
        }
    }

    #[test]
    fn remote_ids_are_namespaced_and_never_look_like_a_local_ones() {
        assert_eq!(project_key("h1", "p1"), "remote:h1:p1");
        assert_eq!(parse_project_key("remote:h1:p1"), Some(("h1", "p1")));
        assert_eq!(folder_key("h1"), "remote:h1");
        assert_eq!(parse_folder_key("remote:h1"), Some("h1"));
        // A host's folder is not a project, and a project is not a folder.
        assert_eq!(parse_project_key("remote:h1"), None);
        assert_eq!(parse_folder_key("remote:h1:p1"), None);
        for odd in [
            "",
            "p1",
            "remote:",
            "remote::p1",
            "remote:h1:",
            "Remote:h1:p1",
        ] {
            assert_eq!(parse_project_key(odd), None, "{odd:?}");
        }
        // Local ids are UUIDs and folder ids are too, so none of them can parse.
        for _ in 0..8 {
            let local = uuid::Uuid::new_v4().to_string();
            assert_eq!(parse_project_key(&local), None);
            assert_eq!(parse_folder_key(&local), None);
        }
    }

    #[test]
    fn a_host_is_a_folder_listing_its_projects_as_rows() {
        let tree = studio();
        let folders = folders(&tree, "");
        let [folder] = folders.as_slice() else {
            panic!("one host is one folder");
        };
        assert_eq!(folder.label, "Studio");
        assert_eq!(folder.link, Some(Link::Online));
        assert_eq!(folder.count, 2);
        assert!(!folder.collapsed && folder.note.is_none() && folder.failure.is_none());
        // Ordered as the local list is: by name here, case-insensitively.
        assert_eq!(names(folder), ["app", "Web"]);
        assert_eq!(folder.projects[0].key, "remote:h1:p1");
        assert!(folder.projects.iter().all(|project| !project.dimmed));
        // Until their lists are read the figures are unknown, not zero.
        assert_eq!(
            folder.projects[0].stats.line(),
            "– trees · –/– tasks · – live"
        );
    }

    #[test]
    fn a_project_row_has_the_same_figures_as_a_local_one() {
        let mut tree = studio();
        for request in [
            Request::Worktrees {
                host: "h1".into(),
                project: "p1".into(),
            },
            Request::Tasks {
                host: "h1".into(),
                project: "p1".into(),
            },
            Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
        ] {
            // The plan creates the project's data; answering without asking is ignored.
            assert!(!tree.apply(&request, Reply::Shells(Ok(vec![]))));
        }
        tree.begin_shell("h1", "p1");
        tree.finish_shell("h1", "p1", Err(rpc("cli_error", "x")));
        let wanted = Wants {
            folders: true,
            ..Wants::default()
        };
        let asked = lists(&mut tree, Instant::now(), &wanted);
        assert!(asked.contains(&Request::Worktrees {
            host: "h1".into(),
            project: "p1".into()
        }));
        tree.apply(
            &Request::Worktrees {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Worktrees(Ok(vec![
                RemoteWorktree {
                    id: "w1".into(),
                    branch: "main".into(),
                    path: "/x".into(),
                    primary: true,
                },
                RemoteWorktree {
                    id: "w2".into(),
                    branch: "feature".into(),
                    path: "/y".into(),
                    primary: false,
                },
            ])),
        );
        tree.apply(
            &Request::Tasks {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Tasks(Ok(vec![
                task("t1", TaskStatus::Done, None),
                task("t2", TaskStatus::Todo, None),
                task("t3", TaskStatus::InProgress, None),
            ])),
        );
        let mut dead = shell("s2", Some("p1"), None);
        dead.alive = false;
        tree.apply(
            &Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Shells(Ok(vec![shell("s1", Some("p1"), None), dead])),
        );
        let folders = folders(&tree, "");
        assert_eq!(
            folders[0].projects[0].stats.line(),
            "2 trees · 1/3 tasks · 1 live"
        );
    }

    #[test]
    fn the_selected_project_is_marked_and_a_collapsed_folder_says_so() {
        let tree = studio();
        let collapsed = HashSet::from([folder_key("h1")]);
        let folders = tree.folders("", by_name(), &collapsed, Some("remote:h1:p2"));
        assert!(folders[0].collapsed);
        assert_eq!(folders[0].count, 2);
        let selected = folders[0]
            .projects
            .iter()
            .filter(|project| project.selected)
            .map(|project| project.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(selected, ["Web"]);
        // A search shows its matches whatever is folded.
        let searched = tree.folders("web", by_name(), &collapsed, None);
        assert!(!searched[0].collapsed);
    }

    #[test]
    fn a_search_filters_projects_and_a_hosts_name_shows_all_of_them() {
        let tree = studio();
        let found = folders(&tree, "web");
        assert_eq!(names(&found[0]), ["Web"]);
        assert_eq!(found[0].count, 2);
        assert_eq!(names(&folders(&tree, "p1")[0]), ["app"]);
        assert_eq!(names(&folders(&tree, "studio")[0]), ["app", "Web"]);
        assert!(folders(&tree, "zzz").is_empty());
    }

    #[test]
    fn projects_follow_the_chosen_order() {
        let tree = studio();
        let by = |order: ProjectOrder| {
            tree.folders("", order, &HashSet::new(), None)[0]
                .projects
                .iter()
                .map(|project| project.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            by(ProjectOrder::for_sort(ProjectSort::DateAdded)),
            ["app", "Web"]
        );
        assert_eq!(
            by(ProjectOrder::for_sort(ProjectSort::DateAdded).toggled()),
            ["Web", "app"]
        );
        assert_eq!(by(by_name().toggled()), ["Web", "app"]);
    }

    #[test]
    fn an_offline_host_keeps_its_last_projects_listed_but_dimmed() {
        let mut tree = studio();
        set_link(&mut tree, LinkState::Offline);
        let folders = folders(&tree, "");
        assert_eq!(folders[0].link, Some(Link::Offline));
        assert_eq!(
            folders[0].note.as_deref(),
            Some("Offline (or access revoked)")
        );
        assert_eq!(names(&folders[0]), ["app", "Web"]);
        assert!(folders[0].projects.iter().all(|project| project.dimmed));

        // A daemon that cannot be reached at all is the same to the person.
        let mut tree = studio();
        tree.apply(
            &Request::Status { host: "h1".into() },
            Reply::Status(Err("no daemon".into())),
        );
        assert_eq!(self::folders(&tree, "")[0].link, Some(Link::Offline));

        // Back on line: normal again.
        set_link(&mut tree, LinkState::Online);
        let folders = self::folders(&tree, "");
        assert!(folders[0].note.is_none());
        assert!(folders[0].projects.iter().all(|project| !project.dimmed));
    }

    #[test]
    fn a_folder_says_why_it_is_empty() {
        let mut tree = RemoteTree::default();
        tree.set_hosts(vec![host("h1", "Studio")]);
        // Nothing heard yet.
        assert_eq!(folders(&tree, "")[0].note.as_deref(), Some("Connecting…"));
        set_link(&mut tree, LinkState::Online);
        assert_eq!(
            folders(&tree, "")[0].note.as_deref(),
            Some("Loading projects…")
        );
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Err("No answer in time".into())),
        );
        assert_eq!(
            folders(&tree, "")[0].note.as_deref(),
            Some("No answer in time")
        );
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(vec![])),
        );
        assert_eq!(folders(&tree, "")[0].note.as_deref(), Some("No projects"));
        // A search that finds nothing in an empty host does not repeat that.
        assert!(folders(&tree, "zzz").is_empty());
    }

    #[test]
    fn projects_kept_from_an_earlier_run_show_at_once_and_are_replaced_by_the_answer() {
        let mut tree = RemoteTree::default();
        tree.keep(BTreeMap::from([(
            "h1".to_owned(),
            vec![project("p1", "app", 20)],
        )]));
        // The registry has not been read: nothing to show yet, and nothing forgotten.
        assert!(folders(&tree, "").is_empty());
        assert_eq!(tree.kept().len(), 1);
        tree.set_hosts(vec![host("h1", "Studio")]);
        assert_eq!(names(&folders(&tree, "")[0]), ["app"]);
        assert!(!tree.take_kept_dirty());
        // Unreachable: the same rows, dimmed.
        set_link(&mut tree, LinkState::Offline);
        let folders_offline = folders(&tree, "");
        assert!(folders_offline[0].projects[0].dimmed);

        // The host answers with something else; that is what is kept now.
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(vec![project("p9", "fresh", 30)])),
        );
        assert!(tree.take_kept_dirty());
        assert!(!tree.take_kept_dirty());
        assert_eq!(tree.kept()["h1"], vec![project("p9", "fresh", 30)]);
        // The same answer again changes nothing worth writing down.
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(vec![project("p9", "fresh", 30)])),
        );
        assert!(!tree.take_kept_dirty());
        // A host that is no longer paired is forgotten once the registry says so.
        tree.set_hosts(vec![]);
        assert!(tree.kept().is_empty());
    }

    #[test]
    fn a_project_is_named_from_what_was_kept_before_its_host_is_known() {
        let mut tree = RemoteTree::default();
        assert_eq!(tree.project_name("h1", "0123456789"), "01234567");
        tree.keep(BTreeMap::from([(
            "h1".to_owned(),
            vec![project("p1", "app", 20)],
        )]));
        assert_eq!(tree.project_name("h1", "p1"), "app");
        assert_eq!(tree.host_name("h1"), "h1");
        tree.set_hosts(vec![host("h1", "Studio")]);
        assert_eq!(tree.project_name("h1", "p1"), "app");
        assert_eq!(tree.host_name("h1"), "Studio");
    }

    #[test]
    fn nothing_is_asked_while_nobody_is_looking() {
        let mut tree = RemoteTree::default();
        assert!(tree.plan(Instant::now(), &Wants::default()).is_empty());
        tree.set_hosts(vec![host("h1", "Studio")]);
        assert!(tree.plan(Instant::now(), &Wants::default()).is_empty());
    }

    #[test]
    fn a_shown_folder_reads_the_registry_then_each_hosts_link_and_repeats_on_a_timer() {
        let now = Instant::now();
        let mut tree = RemoteTree::default();
        let wants = wants_folders();
        assert_eq!(tree.plan(now, &wants), [Request::Hosts]);
        // The request is out: asking again would stack up subprocesses.
        assert!(tree.plan(now, &wants).is_empty());
        tree.apply(
            &Request::Hosts,
            Reply::Hosts(Ok(vec![host("h1", "Studio")])),
        );
        assert_eq!(
            tree.plan(now, &wants),
            [Request::Status { host: "h1".into() }]
        );
        set_link(&mut tree, LinkState::Online);
        // Not due yet: only the project list of the open folder is asked for.
        assert_eq!(
            tree.plan(now + Duration::from_secs(1), &wants),
            [Request::Projects { host: "h1".into() }]
        );
        let later = now + STATUS_INTERVAL;
        assert!(
            tree.plan(later, &wants)
                .contains(&Request::Status { host: "h1".into() })
        );
        set_link(&mut tree, LinkState::Online);
        assert!(
            tree.plan(now + HOSTS_INTERVAL, &wants)
                .contains(&Request::Hosts)
        );
    }

    #[test]
    fn tabs_and_a_selection_alone_watch_their_host_and_read_the_registry_once() {
        let now = Instant::now();
        let mut tree = RemoteTree::default();
        let wants = Wants {
            tab_hosts: BTreeSet::from(["h1".to_owned()]),
            ..Wants::default()
        };
        assert_eq!(tree.plan(now, &wants), [Request::Hosts]);
        tree.apply(
            &Request::Hosts,
            Reply::Hosts(Ok(vec![host("h1", "Studio"), host("h2", "Other")])),
        );
        // Only the host with a tab is watched, and the registry is not read again.
        assert_eq!(
            tree.plan(now, &wants),
            [Request::Status { host: "h1".into() }]
        );
        set_link(&mut tree, LinkState::Online);
        assert_eq!(
            tree.plan(now + HOSTS_INTERVAL * 3, &wants),
            [Request::Status { host: "h1".into() }]
        );
    }

    #[test]
    fn an_open_folder_lists_projects_then_the_figures_a_few_at_a_time() {
        let now = Instant::now();
        let mut tree = RemoteTree::default();
        tree.set_hosts(vec![host("h1", "Studio")]);
        set_link(&mut tree, LinkState::Online);
        let wants = wants_folders();
        assert_eq!(
            lists(&mut tree, now, &wants),
            [Request::Projects { host: "h1".into() }]
        );
        let listed = (1..=6)
            .map(|n| project(&format!("p{n}"), &format!("project {n}"), n))
            .collect::<Vec<_>>();
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(listed)),
        );
        // Three requests per project, but only four out at once for one host.
        let first = lists(&mut tree, now, &wants);
        assert_eq!(first.len(), MAX_STATS_IN_FLIGHT);
        assert!(first.contains(&Request::Worktrees {
            host: "h1".into(),
            project: "p2".into()
        }));
        assert!(!first.contains(&Request::Tasks {
            host: "h1".into(),
            project: "p2".into()
        }));
        // Nothing more while those are out.
        assert!(lists(&mut tree, now, &wants).is_empty());
        answer_all(&mut tree, &first);
        let second = lists(&mut tree, now, &wants);
        assert_eq!(second.len(), MAX_STATS_IN_FLIGHT);
        assert!(second.iter().all(|request| !first.contains(request)));
        // Every figure is asked for once; later they wait for the slow interval.
        answer_all(&mut tree, &second);
        let mut asked = first.len() + second.len();
        loop {
            let more = lists(&mut tree, now, &wants);
            if more.is_empty() {
                break;
            }
            assert!(more.len() <= MAX_STATS_IN_FLIGHT);
            asked += more.len();
            answer_all(&mut tree, &more);
        }
        assert_eq!(asked, 18);
        // Ten seconds on, only the project list itself is asked for again.
        let soon = lists(&mut tree, now + Duration::from_secs(10), &wants);
        assert_eq!(soon, [Request::Projects { host: "h1".into() }]);
        tree.apply(
            &soon[0],
            Reply::Projects(Ok((1..=6)
                .map(|n| project(&format!("p{n}"), &format!("project {n}"), n))
                .collect())),
        );
        let slow = lists(&mut tree, now + STATS_INTERVAL, &wants);
        assert!(
            slow.iter()
                .any(|request| matches!(request, Request::Worktrees { .. }))
        );
    }

    #[test]
    fn a_collapsed_or_hidden_folder_costs_nothing_but_the_link() {
        let now = Instant::now();
        let mut tree = studio();
        let mut wants = wants_folders();
        wants.collapsed.insert(folder_key("h1"));
        assert!(lists(&mut tree, now, &wants).is_empty());
        // The Projects panel is behind another tab.
        assert!(lists(&mut tree, now, &Wants::default()).is_empty());
        wants.collapsed.clear();
        assert!(!lists(&mut tree, now, &wants).is_empty());
    }

    #[test]
    fn the_selected_project_asks_only_for_the_panels_on_screen() {
        let now = Instant::now();
        let mut tree = studio();
        assert_eq!(
            lists(&mut tree, now, &wants_selected("t")),
            [Request::Tasks {
                host: "h1".into(),
                project: "p1".into()
            }]
        );
        let mut tree = studio();
        assert_eq!(
            lists(&mut tree, now, &wants_selected("ws")),
            [
                Request::Worktrees {
                    host: "h1".into(),
                    project: "p1".into()
                },
                Request::Shells {
                    host: "h1".into(),
                    project: "p1".into()
                },
                Request::Orchestrators { host: "h1".into() },
            ]
        );
        // No panel showing, nothing asked: a window that only holds terminals is quiet.
        let mut tree = studio();
        assert!(lists(&mut tree, now, &wants_selected("")).is_empty());
    }

    #[test]
    fn the_selected_projects_lists_are_refreshed_on_the_short_timer() {
        let now = Instant::now();
        let mut tree = studio();
        let wants = wants_selected("s");
        let first = lists(&mut tree, now, &wants);
        answer_all(&mut tree, &first);
        assert!(lists(&mut tree, now + Duration::from_secs(2), &wants).is_empty());
        assert_eq!(lists(&mut tree, now + LIST_INTERVAL, &wants), first);
    }

    #[test]
    fn an_offline_host_is_not_asked_for_lists_and_refreshes_them_when_it_returns() {
        let now = Instant::now();
        let mut tree = studio();
        let wants = wants_selected("t");
        let first = lists(&mut tree, now, &wants);
        answer_all(&mut tree, &first);
        set_link(&mut tree, LinkState::Offline);
        let later = now + Duration::from_secs(1) + LIST_INTERVAL * 2;
        assert!(lists(&mut tree, later, &wants).is_empty());
        // Back online a second later: asked again at once, not at the next timer.
        set_link(&mut tree, LinkState::Online);
        assert_eq!(
            lists(&mut tree, later + Duration::from_secs(1), &wants),
            first
        );
    }

    #[test]
    fn a_shell_creation_is_sent_once_and_never_retried_after_it_fails() {
        let now = Instant::now();
        let mut tree = studio();
        let wants = wants_selected("s");
        let first = lists(&mut tree, now, &wants);
        answer_all(&mut tree, &first);

        assert!(tree.begin_shell("h1", "p1"));
        // A second request while the first is out sends nothing.
        assert!(!tree.begin_shell("h1", "p1"));
        assert!(tree.selected_view("remote:h1:p1").unwrap().creating);

        // The answer never came.
        let t = now + Duration::from_millis(10);
        assert!(
            tree.finish_shell("h1", "p1", Err(RemoteError::Timeout))
                .is_none()
        );
        let view = tree.selected_view("remote:h1:p1").unwrap();
        assert!(!view.creating);
        let message = view.failure.expect("the failure stays");
        assert!(message.contains("may exist"), "{message}");
        // The lost answer makes the list refresh at once so the person can look.
        assert!(lists(&mut tree, t, &wants).contains(&Request::Shells {
            host: "h1".into(),
            project: "p1".into()
        }));
        // However many ticks pass, the failure stays until a person acts.
        for step in 1..=10 {
            let _ = lists(&mut tree, now + Duration::from_secs(step * 7), &wants);
            assert!(
                tree.selected_view("remote:h1:p1")
                    .unwrap()
                    .failure
                    .is_some()
            );
        }
        // Only the person can try again.
        assert!(tree.begin_shell("h1", "p1"));
    }

    #[test]
    fn a_refused_creation_shows_the_hosts_reason_and_leaves_the_list_alone() {
        let now = Instant::now();
        let mut tree = studio();
        let wants = wants_selected("s");
        let first = lists(&mut tree, now, &wants);
        answer_all(&mut tree, &first);
        assert!(tree.begin_shell("h1", "p1"));
        tree.finish_shell(
            "h1",
            "p1",
            Err(rpc(
                "harness_unavailable",
                "codex is not installed or is not on PATH",
            )),
        );
        assert_eq!(
            tree.selected_view("remote:h1:p1")
                .unwrap()
                .failure
                .as_deref(),
            Some("codex is not installed or is not on PATH")
        );
        // A definite refusal created nothing, so the list is not asked for ahead of its timer.
        assert!(lists(&mut tree, now + Duration::from_millis(10), &wants).is_empty());
        tree.dismiss_shell_failure("h1", "p1");
        assert!(
            tree.selected_view("remote:h1:p1")
                .unwrap()
                .failure
                .is_none()
        );
    }

    #[test]
    fn a_daemon_that_could_not_be_reached_sent_nothing_so_nothing_may_exist() {
        let mut tree = studio();
        assert!(tree.begin_shell("h1", "p1"));
        tree.finish_shell(
            "h1",
            "p1",
            Err(RemoteError::Unreachable(
                "Cannot connect to the client daemon for this Mac".into(),
            )),
        );
        assert_eq!(
            tree.selected_view("remote:h1:p1")
                .unwrap()
                .failure
                .as_deref(),
            Some("Cannot connect to the client daemon for this Mac")
        );
    }

    #[test]
    fn a_created_shell_is_handed_back_to_be_opened_and_the_list_refreshes() {
        let now = Instant::now();
        let mut tree = studio();
        let wants = wants_selected("s");
        let first = lists(&mut tree, now, &wants);
        answer_all(&mut tree, &first);
        assert!(tree.begin_shell("h1", "p1"));
        let made = shell("new-shell", Some("p1"), Some("w1"));
        assert_eq!(tree.finish_shell("h1", "p1", Ok(made.clone())), Some(made));
        let view = tree.selected_view("remote:h1:p1").unwrap();
        assert!(!view.creating && view.failure.is_none());
        assert!(
            lists(&mut tree, now + Duration::from_millis(10), &wants).contains(&Request::Shells {
                host: "h1".into(),
                project: "p1".into()
            })
        );
    }

    /// The project's shells, as the host listed them.
    fn with_shells(shells: Vec<RemoteShell>) -> RemoteTree {
        let mut tree = studio();
        let first = lists(&mut tree, Instant::now(), &wants_selected("s"));
        answer_all(&mut tree, &first);
        tree.apply(
            &Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Shells(Ok(shells)),
        );
        tree
    }

    fn click(tree: &RemoteTree, open_tabs: &[&str]) -> WorktreeClick {
        tree.click_worktree("h1", "p1", "w1", open_tabs)
    }

    #[test]
    fn a_worktree_with_no_shell_starts_a_plain_one_as_a_local_worktree_does() {
        // Another worktree's shell, another project's, and a dead one in this worktree do not
        // count; only a live shell of this worktree does.
        let mut dead = shell("dead", Some("p1"), Some("w1"));
        dead.alive = false;
        let tree = with_shells(vec![
            shell("elsewhere", Some("p1"), Some("w2")),
            shell("rootless", Some("p1"), None),
            shell("foreign", Some("p2"), Some("w1")),
            dead,
        ]);
        assert_eq!(click(&tree, &[]), WorktreeClick::Start);
        assert_eq!(click(&with_shells(vec![]), &[]), WorktreeClick::Start);
    }

    #[test]
    fn a_worktree_with_a_live_shell_opens_the_newest_one_of_any_kind() {
        let mut agent = shell("agent", Some("p1"), Some("w1"));
        agent.harness = Some("codex".into());
        let mut orchestrator = shell("orch", Some("p1"), Some("w1"));
        orchestrator.orchestrator = true;
        let tree = with_shells(vec![
            shell("old", Some("p1"), Some("w1")),
            agent.clone(),
            shell("elsewhere", Some("p1"), Some("w2")),
        ]);
        // As the local rule takes the last live one in its list, whatever runs in it.
        assert_eq!(click(&tree, &[]), WorktreeClick::Live(agent));
        let tree = with_shells(vec![
            shell("old", Some("p1"), Some("w1")),
            orchestrator.clone(),
        ]);
        assert_eq!(click(&tree, &[]), WorktreeClick::Live(orchestrator));
    }

    #[test]
    fn a_tab_already_open_on_a_shell_of_the_worktree_is_shown_before_anything_is_started() {
        let mut dead = shell("dead", Some("p1"), Some("w1"));
        dead.alive = false;
        let tree = with_shells(vec![
            shell("live", Some("p1"), Some("w1")),
            dead,
            shell("other", Some("p1"), Some("w2")),
        ]);
        // A tab on a shell of another worktree is not this worktree's.
        assert_eq!(
            click(&tree, &["other"]),
            WorktreeClick::Live(shell("live", Some("p1"), Some("w1")))
        );
        // The tab comes first, even over a live shell and even if its own shell has ended,
        // as a local tab on an ended shell is.
        assert_eq!(
            click(&tree, &["other", "dead"]),
            WorktreeClick::Tab("dead".into())
        );
        let tree = with_shells(vec![]);
        assert_eq!(click(&tree, &["unlisted"]), WorktreeClick::Start);
    }

    #[test]
    fn a_shell_just_made_is_known_by_its_worktree_before_the_list_names_it() {
        let mut tree = with_shells(vec![]);
        assert!(tree.begin_shell("h1", "p1"));
        let made = shell("made", Some("p1"), Some("w1"));
        tree.finish_shell("h1", "p1", Ok(made));
        // Its tab is open but the list has not caught up: the click shows the tab and does not
        // start a second shell.
        assert_eq!(click(&tree, &["made"]), WorktreeClick::Tab("made".into()));
    }

    #[test]
    fn without_the_hosts_shells_a_click_starts_nothing() {
        let mut tree = studio();
        assert_eq!(click(&tree, &[]), WorktreeClick::Unknown);
        // A list that failed to load is just as unknown.
        let first = lists(&mut tree, Instant::now(), &wants_selected("s"));
        assert!(!first.is_empty());
        tree.apply(
            &Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Shells(Err("No answer in time".into())),
        );
        assert_eq!(click(&tree, &[]), WorktreeClick::Unknown);
    }

    #[test]
    fn project_creation_follows_the_same_rules_per_host() {
        let mut tree = studio();
        assert!(tree.begin_project("h1"));
        assert!(!tree.begin_project("h1"));
        assert!(!tree.begin_project("unknown"));
        assert!(folders(&tree, "")[0].creating);
        // "already_exists" is the host's own sentence, shown as is.
        tree.finish_project(
            "h1",
            Err(rpc(
                "already_exists",
                "A project named \"fresh\" already exists on the desktop",
            )),
        );
        let folder = &folders(&tree, "")[0];
        assert!(!folder.creating);
        assert!(
            folder
                .failure
                .as_deref()
                .is_some_and(|message| message.contains("already exists"))
        );
        tree.dismiss_project_failure("h1");
        assert!(folders(&tree, "")[0].failure.is_none());

        assert!(tree.begin_project("h1"));
        let created = project("p9", "fresh", 40);
        assert_eq!(
            tree.finish_project("h1", Ok(created.clone())),
            Some(created)
        );
        // It joins the folder at the next answer from the host, which is asked for at once.
        let asked = lists(&mut tree, Instant::now(), &wants_folders());
        assert!(asked.contains(&Request::Projects { host: "h1".into() }));
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(vec![
                project("p1", "app", 20),
                project("p2", "Web", 10),
                project("p9", "fresh", 40),
            ])),
        );
        assert_eq!(names(&folders(&tree, "")[0]), ["app", "fresh", "Web"]);

        // A lost reply: the failure says the project may exist, and nothing is sent again.
        assert!(tree.begin_project("h1"));
        tree.finish_project(
            "h1",
            Err(RemoteError::Protocol(
                "the daemon closed the connection".into(),
            )),
        );
        assert!(
            folders(&tree, "")[0]
                .failure
                .as_deref()
                .is_some_and(|message| message.contains("may exist"))
        );
        for step in 1..=5 {
            let _ = lists(
                &mut tree,
                Instant::now() + Duration::from_secs(step * 20),
                &wants_folders(),
            );
            assert!(folders(&tree, "")[0].failure.is_some());
        }
    }

    #[test]
    fn an_old_host_that_lacks_the_method_is_told_to_update() {
        let mut tree = studio();
        assert!(tree.begin_project("h1"));
        tree.finish_project("h1", Err(rpc("invalid_request", "unsupported RPC method")));
        assert!(
            folders(&tree, "")[0]
                .failure
                .as_deref()
                .is_some_and(|message| message.contains("Update"))
        );
    }

    #[test]
    fn the_selected_view_lists_orchestrators_before_shells_and_never_another_projects() {
        let mut tree = studio();
        let mut project_orchestrator = shell("o1", Some("p1"), None);
        project_orchestrator.orchestrator = true;
        let mut other_orchestrator = shell("o2", Some("p2"), None);
        other_orchestrator.orchestrator = true;
        let mut global = shell("og", None, None);
        global.orchestrator = true;
        let wants = wants_selected("wts");
        let asked = lists(&mut tree, Instant::now(), &wants);
        assert_eq!(asked.len(), 4);
        tree.apply(
            &Request::Orchestrators { host: "h1".into() },
            Reply::Orchestrators(Ok(vec![global, other_orchestrator, project_orchestrator])),
        );
        tree.apply(
            &Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Shells(Ok(vec![shell("s1", Some("p1"), Some("w1"))])),
        );
        tree.apply(
            &Request::Tasks {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Tasks(Ok(vec![task("t1", TaskStatus::Todo, Some("w1"))])),
        );
        let view = tree
            .selected_view("remote:h1:p1")
            .expect("a remote project");
        assert_eq!(view.host_label, "Studio");
        assert_eq!(view.project_name, "app");
        assert_eq!(view.link, Some(Link::Online));
        assert!(!view.unpaired);
        let ids = view
            .shells
            .items
            .iter()
            .map(|shell| shell.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["o1", "og", "s1"]);
        assert_eq!(view.tasks.items.len(), 1);
        assert!(view.worktrees.loading);
        // The host's own orchestrators can be opened from the menu once listed.
        assert_eq!(
            tree.orchestrator("h1", Some("p1")).map(|s| s.id.as_str()),
            Some("o1")
        );
        assert_eq!(
            tree.orchestrator("h1", None).map(|s| s.id.as_str()),
            Some("og")
        );
        assert_eq!(tree.live_shells("h1", "p1"), Some(1));
    }

    #[test]
    fn the_selected_view_survives_an_unknown_missing_or_unpaired_host() {
        let mut tree = RemoteTree::default();
        // Nothing but the id yet: the window must still draw something calm.
        let view = tree.selected_view("remote:h1:p1").expect("view");
        assert!(!view.unpaired && view.link.is_none());
        assert_eq!(view.host_label, "h1");
        // The registry was read and the host is not in it.
        tree.set_hosts(vec![host("other", "Other")]);
        assert!(tree.selected_view("remote:h1:p1").unwrap().unpaired);
        // A local id is never a remote view.
        assert!(
            tree.selected_view("5c0d2c3e-0000-4000-8000-000000000000")
                .is_none()
        );
        assert!(tree.selected_view("remote:h1").is_none());
    }

    #[test]
    fn create_requests_name_the_scope_and_only_agents_can_be_unrestricted() {
        let project = ShellScope::Project("p1".into());
        let worktree = ShellScope::Worktree("w1".into());
        assert_eq!(
            shell_create_params(&project, NewShellKind::Shell, false),
            json!({"project_id": "p1", "kind": "shell"})
        );
        assert_eq!(
            shell_create_params(&worktree, NewShellKind::Claude, false),
            json!({"worktree_id": "w1", "kind": "claude"})
        );
        assert_eq!(
            shell_create_params(&project, NewShellKind::Codex, true),
            json!({"project_id": "p1", "kind": "codex", "unrestricted": true})
        );
        // The host refuses the flag on a plain shell, so it is not sent.
        assert_eq!(
            shell_create_params(&project, NewShellKind::Shell, true),
            json!({"project_id": "p1", "kind": "shell"})
        );
        let kinds = [
            NewShellKind::Shell,
            NewShellKind::Codex,
            NewShellKind::Claude,
            NewShellKind::Grok,
        ]
        .map(NewShellKind::wire);
        assert_eq!(kinds, ["shell", "codex", "claude", "grok"]);
        assert_eq!(project_create_params("App"), json!({"name": "App"}));
    }

    #[test]
    fn project_names_are_checked_like_the_host_checks_them() {
        assert_eq!(validate_project_name("  My App ").unwrap(), "My App");
        assert_eq!(
            validate_project_name("Scratch; $HOME 🙂").unwrap(),
            "Scratch; $HOME 🙂"
        );
        for bad in [
            "",
            "   ",
            ".git",
            "a/b",
            "a\\b",
            "-rf",
            "tab\there",
            "x\u{2028}y",
        ] {
            assert!(validate_project_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_project_name(&"n".repeat(100)).is_ok());
        assert!(validate_project_name(&"n".repeat(101)).is_err());
        // 100 characters can still be over 255 bytes.
        assert!(validate_project_name(&"🙂".repeat(70)).is_err());
    }

    #[test]
    fn replies_parse_leniently_and_report_a_missing_list() {
        let projects = parse_projects(&json!({"projects": [
            {"id": "p1", "name": "app", "root": "/x", "created_at": 5, "extra": true},
            {"name": "no id"},
        ]}))
        .unwrap();
        assert_eq!(projects, [project("p1", "app", 5)]);
        assert!(parse_projects(&json!({})).is_err());
        let worktrees = parse_worktrees(&json!({"worktrees": [
            {"id": "w1", "project_id": "p1", "branch": "main", "path": "/x", "is_primary": true},
        ]}))
        .unwrap();
        assert_eq!(worktrees[0].branch, "main");
        assert_eq!(worktrees[0].path, "/x");
        assert!(worktrees[0].primary);
        let tasks = parse_tasks(&json!({"tasks": [
            {"id": "t1", "project_id": "p1", "title": "Fix", "details": "d", "status": "in_progress",
             "worktree_id": null, "created_at": 1, "updated_at": 2},
            {"id": "t2", "title": "Odd", "status": "from the future"},
        ]}))
        .unwrap();
        assert_eq!(tasks[0].status, TaskStatus::InProgress);
        assert_eq!(tasks[0].worktree_id, None);
        assert_eq!(tasks[1].status, TaskStatus::Todo);
        let shells = parse_shells(&json!({"shells": [
            {"id": "s1", "project_id": "p1", "worktree_id": null, "kind": "project",
             "cwd": "/x", "harness": null, "alive": true, "created_at_unix": 1},
        ]}))
        .unwrap();
        assert_eq!(shells[0].worktree_id, None);
        assert!(!shells[0].orchestrator && shells[0].alive);
        let orchestrators = parse_orchestrators(&json!({"orchestrators": [
            {"id": "o1", "project_id": null, "kind": "orchestrator", "harness": "codex", "alive": false},
        ]}))
        .unwrap();
        assert!(orchestrators[0].orchestrator && !orchestrators[0].alive && !orchestrators[0].chat);
        // An orchestrator that runs as a chat on its host has no terminal to attach to.
        let orchestrators = parse_orchestrators(&json!({"orchestrators": [
            {"id": "o1", "kind": "orchestrator", "harness": "codex", "alive": true, "mode": "terminal"},
            {"id": "o2", "kind": "orchestrator", "harness": "claude", "alive": true,
             "mode": "chat", "chat_id": "o2", "provider": "claude"},
        ]}))
        .unwrap();
        assert!(!orchestrators[0].chat && orchestrators[1].chat);
        assert_eq!(orchestrators[0].display(), "orchestrator (codex) · o1");
        assert_eq!(
            orchestrators[1].display(),
            "orchestrator chat (claude) · o2"
        );
        let created =
            parse_created_shell(&json!({"shell_id": "s1", "shell": {"id": "s1", "alive": true}}))
                .unwrap();
        assert_eq!(created.id, "s1");
        assert!(parse_created_shell(&json!({"shell_id": "s1"})).is_err());
        let made = parse_created_project(
            &json!({"project_id": "p1", "project": {"id": "p1", "name": "App"}}),
        )
        .unwrap();
        assert_eq!(made.name, "App");
    }

    #[test]
    fn remote_tab_titles_carry_the_host_mark_and_split_back_into_it() {
        let title = remote_tab_title("MacBook", "codex · 1a2b3c4d");
        assert_eq!(title, "⇄ MacBook · codex · 1a2b3c4d");
        assert_eq!(
            split_remote_title(&title),
            Some(("⇄ MacBook · ", "codex · 1a2b3c4d"))
        );
        assert_eq!(split_remote_title("zsh 01 · main"), None);
        assert_eq!(split_remote_title("⇄ no separator"), None);
    }

    #[test]
    fn tabs_are_titled_from_what_the_lists_know_and_otherwise_from_ids() {
        let mut tree = studio();
        let wants = wants_selected("s");
        let asked = lists(&mut tree, Instant::now(), &wants);
        assert!(!asked.is_empty());
        let mut claude = shell("s-claude-0001", Some("p1"), None);
        claude.harness = Some("claude".into());
        tree.apply(
            &Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Shells(Ok(vec![claude])),
        );
        assert_eq!(
            tree.tab_title("h1", "s-claude-0001"),
            "⇄ Studio · claude · s-claude"
        );
        assert_eq!(
            tree.tab_title("h1", "unlisted-shell"),
            "⇄ Studio · unlisted"
        );
        // A host the registry no longer has is named by the start of its id.
        assert_eq!(
            tree.tab_title("11111111-2222", "unlisted-shell"),
            "⇄ 11111111 · unlisted"
        );
    }

    #[test]
    fn the_reconnecting_strip_shows_only_while_the_link_is_down() {
        assert_eq!(link_strip("Studio", None), None);
        assert_eq!(link_strip("Studio", Some(Link::Online)), None);
        assert_eq!(
            link_strip("Studio", Some(Link::Connecting)).as_deref(),
            Some("RECONNECTING · Studio")
        );
        assert!(
            link_strip("Studio", Some(Link::Offline))
                .unwrap()
                .starts_with("RECONNECTING · Studio")
        );
        let mut tree = studio();
        assert_eq!(tree.strip_for("h1"), None);
        set_link(&mut tree, LinkState::Connecting);
        assert_eq!(
            tree.strip_for("h1").as_deref(),
            Some("RECONNECTING · Studio")
        );
        assert_eq!(tree.strip_for("gone"), None);
    }

    #[test]
    fn answers_for_a_vanished_host_or_the_wrong_request_are_ignored() {
        let mut tree = studio();
        assert!(!tree.apply(
            &Request::Projects {
                host: "gone".into()
            },
            Reply::Projects(Ok(vec![]))
        ));
        assert!(!tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Shells(Ok(vec![]))
        ));
    }
}
