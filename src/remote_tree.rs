//! What the REMOTE section of the Projects panel knows about other Macs, and when it asks.
//!
//! There is no GPUI and no I/O here. `RemoteTree` is plain state that background tasks fill
//! in, so the decisions (which list is wanted now, which request may start, what a failed
//! creation leaves behind) are tested without a window or a daemon. `remote_service` runs
//! the requests this module plans and feeds the answers back.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::remote_hosts::{Host, HostStatus, LinkState, RemoteError};

/// How often a host's link state is asked for while someone is looking at it.
pub const STATUS_INTERVAL: Duration = Duration::from_secs(3);
/// How often an open list is asked for again.
pub const LIST_INTERVAL: Duration = Duration::from_secs(5);
/// How often the registry of hosts is read again while it is shown.
pub const HOSTS_INTERVAL: Duration = Duration::from_secs(10);
/// A list reply travels through the relay and the host's CLI; this is generous.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// Starting an agent can take a minute on the host (docs/remote-protocol.md allows 90 s).
pub const CREATE_TIMEOUT: Duration = Duration::from_secs(90);

/// The longest project name the host accepts (Unicode scalar values).
const PROJECT_NAME_LIMIT: usize = 100;

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

    /// Record an answer. Returns whether what the panel shows changed.
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
    pub const ALL: [Self; 4] = [Self::Shell, Self::Codex, Self::Claude, Self::Grok];

    /// Spelled exactly as the protocol wants it.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Grok => "grok",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Shell => "Shell",
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Grok => "Grok",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteProject {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteWorktree {
    pub id: String,
    pub branch: String,
    pub primary: bool,
}

/// A project shell or an orchestrator on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteShell {
    pub id: String,
    pub project_id: Option<String>,
    pub worktree_id: Option<String>,
    pub orchestrator: bool,
    pub harness: Option<String>,
    pub alive: bool,
    pub cwd: String,
}

impl RemoteShell {
    /// A short name for a row or a tab: what runs in it, and which one it is.
    pub fn display(&self) -> String {
        let what = match (self.orchestrator, self.harness.as_deref()) {
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

/// What a creation request left behind. It is never retried by the panel: the user decides.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Creation {
    #[default]
    Idle,
    Creating,
    Failed(String),
}

#[derive(Debug)]
struct ProjectNode {
    expanded: bool,
    worktrees: Fetch<Vec<RemoteWorktree>>,
    shells: Fetch<Vec<RemoteShell>>,
    /// The Shell/Codex/Claude/Grok choice is showing under the project.
    chooser_open: bool,
    create: Creation,
}

impl ProjectNode {
    fn new() -> Self {
        Self {
            expanded: false,
            worktrees: Fetch::default(),
            shells: Fetch::default(),
            chooser_open: false,
            create: Creation::Idle,
        }
    }
}

#[derive(Debug)]
struct HostNode {
    host: Host,
    /// `None` until the first answer; an error means the daemon could not be reached.
    status: Option<Result<HostStatus, String>>,
    status_poll: Poll,
    expanded: bool,
    projects: Fetch<Vec<RemoteProject>>,
    orchestrators: Fetch<Vec<RemoteShell>>,
    nodes: BTreeMap<String, ProjectNode>,
    create: Creation,
}

impl HostNode {
    fn new(host: Host) -> Self {
        Self {
            host,
            status: None,
            status_poll: Poll::default(),
            expanded: false,
            projects: Fetch::default(),
            orchestrators: Fetch::default(),
            nodes: BTreeMap::new(),
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
        for node in self.nodes.values_mut() {
            node.worktrees.poll.invalidate();
            node.shells.poll.invalidate();
        }
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
    Shells { host: String, project: String },
}

/// The answer to a [`Request`], already parsed. The error is text for the panel.
#[derive(Debug)]
pub enum Reply {
    Hosts(Result<Vec<Host>, String>),
    Status(Result<HostStatus, String>),
    Projects(Result<Vec<RemoteProject>, String>),
    Orchestrators(Result<Vec<RemoteShell>, String>),
    Worktrees(Result<Vec<RemoteWorktree>, String>),
    Shells(Result<Vec<RemoteShell>, String>),
}

/// What is on screen and so worth keeping up to date.
#[derive(Clone, Debug, Default)]
pub struct Wants {
    /// The REMOTE section of a Projects panel is showing.
    pub section: bool,
    /// A Settings panel listing the hosts is showing.
    pub settings: bool,
    /// Hosts that tabs are open on; their link state drives the reconnecting strip.
    pub tab_hosts: BTreeSet<String>,
}

/// A row of the REMOTE section, flattened for drawing. `depth` is the indentation level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Host {
        id: String,
        label: String,
        link: Option<Link>,
        expanded: bool,
        creating: bool,
    },
    Project {
        host: String,
        id: String,
        name: String,
        expanded: bool,
        creating: bool,
    },
    /// The Shell/Codex/Claude/Grok choice under a project.
    Chooser {
        host: String,
        project: String,
    },
    Worktree {
        label: String,
    },
    Shell {
        host: String,
        shell: RemoteShell,
        depth: usize,
    },
    /// A line of text under its parent: loading, empty, or an error.
    Note {
        text: String,
        error: bool,
        depth: usize,
    },
    /// What a creation left behind, with a way to dismiss it. `project` is `None` for a
    /// project creation on the host itself.
    Failure {
        host: String,
        project: Option<String>,
        text: String,
        depth: usize,
    },
}

#[derive(Debug, Default)]
pub struct RemoteTree {
    hosts: Vec<HostNode>,
    /// The registry has been read at least once.
    hosts_poll: Poll,
    hosts_error: Option<String>,
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

    /// The strip a tab on `host` shows, if any.
    pub fn strip_for(&self, host: &str) -> Option<String> {
        let node = self.node(host)?;
        link_strip(&node.host.label, node.link())
    }

    /// The shell as the host last listed it, if a list this window loaded has it.
    pub fn known_shell(&self, host: &str, shell_id: &str) -> Option<&RemoteShell> {
        let node = self.node(host)?;
        node.orchestrators
            .value()
            .into_iter()
            .flatten()
            .chain(
                node.nodes
                    .values()
                    .filter_map(|project| project.shells.value())
                    .flatten(),
            )
            .find(|shell| shell.id == shell_id)
    }

    /// The title for a tab on `shell_id` of `host`: the shell's kind when a list knows it,
    /// otherwise its short id.
    pub fn tab_title(&self, host: &str, shell_id: &str) -> String {
        let label = self.host_label(host).unwrap_or_else(|| short_id(host));
        let detail = self
            .known_shell(host, shell_id)
            .map(RemoteShell::display)
            .unwrap_or_else(|| short_id(shell_id).to_owned());
        remote_tab_title(label, &detail)
    }

    fn node(&self, id: &str) -> Option<&HostNode> {
        self.hosts.iter().find(|node| node.host.id == id)
    }

    fn node_mut(&mut self, id: &str) -> Option<&mut HostNode> {
        self.hosts.iter_mut().find(|node| node.host.id == id)
    }

    /// Replace the registry's hosts. Hosts that stay keep what was loaded for them.
    /// Returns whether anything changed.
    pub fn set_hosts(&mut self, hosts: Vec<Host>) -> bool {
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
                    None => HostNode::new(host),
                },
            )
            .collect();
        true
    }

    /// Read the registry again at the next plan, as after a host was added or removed.
    pub fn invalidate_hosts(&mut self) {
        self.hosts_poll.invalidate();
    }

    pub fn toggle_host(&mut self, id: &str) {
        if let Some(node) = self.node_mut(id) {
            node.expanded = !node.expanded;
        }
    }

    pub fn toggle_project(&mut self, host: &str, project: &str) {
        if let Some(node) = self.node_mut(host) {
            let project = node
                .nodes
                .entry(project.to_owned())
                .or_insert_with(ProjectNode::new);
            project.expanded = !project.expanded;
            if !project.expanded {
                project.chooser_open = false;
            }
        }
    }

    /// Open the project so its worktrees and shells are listed.
    pub fn expand_project(&mut self, host: &str, project: &str) {
        if let Some(node) = self.node_mut(host) {
            node.expanded = true;
            node.nodes
                .entry(project.to_owned())
                .or_insert_with(ProjectNode::new)
                .expanded = true;
        }
    }

    pub fn toggle_chooser(&mut self, host: &str, project: &str) {
        if let Some(node) = self.node_mut(host) {
            let project = node
                .nodes
                .entry(project.to_owned())
                .or_insert_with(ProjectNode::new);
            project.chooser_open = !project.chooser_open;
            // A refused creation is read once; reopening the choice starts clean.
            if project.chooser_open && matches!(project.create, Creation::Failed(_)) {
                project.create = Creation::Idle;
            }
        }
    }

    /// Dismiss the message a failed creation left under a project, or under the host when
    /// `project` is `None`.
    pub fn dismiss_failure(&mut self, host: &str, project: Option<&str>) {
        let Some(node) = self.node_mut(host) else {
            return;
        };
        let slot = match project {
            Some(project) => node
                .nodes
                .get_mut(project)
                .map(|project| &mut project.create),
            None => Some(&mut node.create),
        };
        if let Some(slot) = slot.filter(|slot| matches!(slot, Creation::Failed(_))) {
            *slot = Creation::Idle;
        }
    }

    /// Decide what to ask for now, and mark those requests as running. Nothing is planned
    /// for something nobody is looking at, so a closed section or a collapsed host costs no
    /// traffic. Creations are never planned here: only a person starts one.
    pub fn plan(&mut self, now: Instant, wants: &Wants) -> Vec<Request> {
        let mut requests = Vec::new();
        let looking = wants.section || wants.settings;
        let has_tabs = !wants.tab_hosts.is_empty();
        if looking || has_tabs {
            // Tabs alone need the labels once; a shown list is kept current.
            let every = looking.then_some(HOSTS_INTERVAL);
            if self.hosts_poll.due(now, every) {
                self.hosts_poll.begin(now);
                requests.push(Request::Hosts);
            }
        }
        for node in &mut self.hosts {
            let host = node.host.id.clone();
            let watched = looking || wants.tab_hosts.contains(&host);
            if watched && node.status_poll.due(now, Some(STATUS_INTERVAL)) {
                node.status_poll.begin(now);
                requests.push(Request::Status { host: host.clone() });
            }
            if !wants.section || !node.expanded || !node.reachable() {
                continue;
            }
            if node.projects.poll.due(now, Some(LIST_INTERVAL)) {
                node.projects.poll.begin(now);
                requests.push(Request::Projects { host: host.clone() });
            }
            if node.orchestrators.poll.due(now, Some(LIST_INTERVAL)) {
                node.orchestrators.poll.begin(now);
                requests.push(Request::Orchestrators { host: host.clone() });
            }
            for (id, project) in &mut node.nodes {
                if !project.expanded {
                    continue;
                }
                if project.worktrees.poll.due(now, Some(LIST_INTERVAL)) {
                    project.worktrees.poll.begin(now);
                    requests.push(Request::Worktrees {
                        host: host.clone(),
                        project: id.clone(),
                    });
                }
                if project.shells.poll.due(now, Some(LIST_INTERVAL)) {
                    project.shells.poll.begin(now);
                    requests.push(Request::Shells {
                        host: host.clone(),
                        project: id.clone(),
                    });
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
            (Request::Projects { host }, Reply::Projects(result)) => self
                .node_mut(host)
                .is_some_and(|node| node.projects.finish(result)),
            (Request::Orchestrators { host }, Reply::Orchestrators(result)) => self
                .node_mut(host)
                .is_some_and(|node| node.orchestrators.finish(result)),
            (Request::Worktrees { host, project }, Reply::Worktrees(result)) => self
                .node_mut(host)
                .and_then(|node| node.nodes.get_mut(project))
                .is_some_and(|project| project.worktrees.finish(result)),
            (Request::Shells { host, project }, Reply::Shells(result)) => self
                .node_mut(host)
                .and_then(|node| node.nodes.get_mut(project))
                .is_some_and(|project| project.shells.finish(result)),
            // An answer for something else is a bug in the caller; ignoring it is safest.
            _ => false,
        }
    }

    /// The user asked for a new shell. Returns whether the request may be sent: not while
    /// another creation for that project is still out, and not for a host or project this
    /// window does not know. Sending it is the caller's job, exactly once.
    pub fn begin_shell(&mut self, host: &str, project: &str) -> bool {
        let Some(node) = self
            .node_mut(host)
            .and_then(|node| node.nodes.get_mut(project))
        else {
            return false;
        };
        if node.create == Creation::Creating {
            return false;
        }
        node.create = Creation::Creating;
        node.chooser_open = false;
        true
    }

    /// Record how a shell creation ended. On success the new shell is returned so it can be
    /// opened. After an answer that may not have been the whole story (no reply in time, a
    /// lost connection) the list is refreshed so the person can see whether the shell exists,
    /// and nothing is retried.
    pub fn finish_shell(
        &mut self,
        host: &str,
        project: &str,
        result: Result<RemoteShell, RemoteError>,
    ) -> Option<RemoteShell> {
        let label = self.host_label(host).unwrap_or("the host").to_owned();
        let node = self
            .node_mut(host)
            .and_then(|node| node.nodes.get_mut(project))?;
        match result {
            Ok(shell) => {
                node.create = Creation::Idle;
                node.shells.poll.invalidate();
                Some(shell)
            }
            Err(error) => {
                node.create = Creation::Failed(creation_failure(&error, &label, "shell"));
                if outcome_unknown(&error) {
                    node.shells.poll.invalidate();
                }
                None
            }
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

    /// Record how a project creation ended; the new project is returned on success and
    /// opened in the list.
    pub fn finish_project(
        &mut self,
        host: &str,
        result: Result<RemoteProject, RemoteError>,
    ) -> Option<RemoteProject> {
        let label = self.host_label(host).unwrap_or("the host").to_owned();
        let node = self.node_mut(host)?;
        match result {
            Ok(project) => {
                node.create = Creation::Idle;
                node.projects.poll.invalidate();
                node.expanded = true;
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

    /// The rows to draw for `query` (already lowercased and trimmed; empty shows
    /// everything). A search looks at what has been loaded and opens the hosts and projects
    /// that hold a match.
    pub fn rows(&self, query: &str) -> Vec<Row> {
        let mut rows = Vec::new();
        for node in &self.hosts {
            node.push_rows(query, &mut rows);
        }
        rows
    }
}

#[cfg(test)]
impl RemoteTree {
    fn link(&self, id: &str) -> Option<Link> {
        self.node(id).and_then(HostNode::link)
    }

    fn creation(&self, host: &str, project: Option<&str>) -> Option<&Creation> {
        let node = self.node(host)?;
        match project {
            Some(project) => node.nodes.get(project).map(|project| &project.create),
            None => Some(&node.create),
        }
    }
}

impl HostNode {
    fn push_rows(&self, query: &str, rows: &mut Vec<Row>) {
        let searching = !query.is_empty();
        let host_matches = !searching || self.host.label.to_lowercase().contains(query);
        let found = self.matches(query);
        if searching && !host_matches && !found {
            return;
        }
        // A search shows the matches themselves, so what is closed opens for it.
        let expanded = self.expanded || (searching && found);
        rows.push(Row::Host {
            id: self.host.id.clone(),
            label: self.host.label.clone(),
            link: self.link(),
            expanded,
            creating: self.create == Creation::Creating,
        });
        if let Creation::Failed(message) = &self.create {
            rows.push(Row::Failure {
                host: self.host.id.clone(),
                project: None,
                text: message.clone(),
                depth: 1,
            });
        }
        if !expanded {
            return;
        }
        match self.link() {
            Some(Link::Online) => {}
            Some(Link::Offline) => {
                rows.push(note(Link::Offline.text(), false, 1));
                return;
            }
            Some(Link::Connecting) | None => {
                rows.push(note(Link::Connecting.text(), false, 1));
                return;
            }
        }
        let show_all = host_matches;
        match self.projects.value() {
            None => rows.push(match self.projects.error() {
                Some(error) => note(error, true, 1),
                None => note("Loading projects…", false, 1),
            }),
            Some(projects) => {
                let orchestrators = self.orchestrators.value();
                // Orchestrators that belong to no project are the host's own.
                let global = orchestrators
                    .into_iter()
                    .flatten()
                    .filter(|shell| shell.project_id.is_none())
                    .filter(|shell| show_all || shell_matches(shell, query))
                    .collect::<Vec<_>>();
                for shell in global {
                    rows.push(Row::Shell {
                        host: self.host.id.clone(),
                        shell: shell.clone(),
                        depth: 1,
                    });
                }
                let mut shown = 0;
                for project in projects {
                    let node = self.nodes.get(&project.id);
                    if !show_all
                        && !project.name.to_lowercase().contains(query)
                        && !node.is_some_and(|node| self.project_matches(&project.id, node, query))
                    {
                        continue;
                    }
                    shown += 1;
                    let project_expanded =
                        node.is_some_and(|node| node.expanded || (searching && !show_all));
                    rows.push(Row::Project {
                        host: self.host.id.clone(),
                        id: project.id.clone(),
                        name: project.name.clone(),
                        expanded: project_expanded,
                        creating: node.is_some_and(|node| node.create == Creation::Creating),
                    });
                    if let Some(node) = node {
                        self.push_project_rows(project, node, project_expanded, query, rows);
                    }
                }
                if shown == 0 && !searching {
                    rows.push(note("No projects", false, 1));
                }
                if let Some(error) = self.projects.error() {
                    rows.push(note(error, true, 1));
                }
            }
        }
    }

    fn push_project_rows(
        &self,
        project: &RemoteProject,
        node: &ProjectNode,
        expanded: bool,
        query: &str,
        rows: &mut Vec<Row>,
    ) {
        if node.chooser_open {
            rows.push(Row::Chooser {
                host: self.host.id.clone(),
                project: project.id.clone(),
            });
        }
        if let Creation::Failed(message) = &node.create {
            rows.push(Row::Failure {
                host: self.host.id.clone(),
                project: Some(project.id.clone()),
                text: message.clone(),
                depth: 2,
            });
        }
        if !expanded {
            return;
        }
        let show_all = query.is_empty() || project.name.to_lowercase().contains(query);
        let keep = |shell: &&RemoteShell| show_all || shell_matches(shell, query);
        let orchestrators = self
            .orchestrators
            .value()
            .into_iter()
            .flatten()
            .filter(|shell| shell.project_id.as_deref() == Some(project.id.as_str()))
            .filter(keep);
        for shell in orchestrators {
            rows.push(Row::Shell {
                host: self.host.id.clone(),
                shell: shell.clone(),
                depth: 2,
            });
        }
        let shells = node.shells.value();
        let worktrees = node.worktrees.value();
        if let Some(worktrees) = worktrees {
            for worktree in worktrees {
                let own = shells
                    .into_iter()
                    .flatten()
                    .filter(|shell| shell.worktree_id.as_deref() == Some(worktree.id.as_str()))
                    .filter(keep)
                    .collect::<Vec<_>>();
                let branch_matches = show_all || worktree.branch.to_lowercase().contains(query);
                if !branch_matches && own.is_empty() {
                    continue;
                }
                rows.push(Row::Worktree {
                    label: format!(
                        "{} {}",
                        if worktree.primary { "◆" } else { "◇" },
                        worktree.branch
                    ),
                });
                for shell in own {
                    rows.push(Row::Shell {
                        host: self.host.id.clone(),
                        shell: shell.clone(),
                        depth: 3,
                    });
                }
            }
        }
        // Shells the host did not place in a listed worktree still need a row.
        let known = |shell: &RemoteShell| {
            worktrees.is_some_and(|worktrees| {
                worktrees
                    .iter()
                    .any(|worktree| shell.worktree_id.as_deref() == Some(worktree.id.as_str()))
            })
        };
        let loose = shells
            .into_iter()
            .flatten()
            .filter(|shell| !known(shell))
            .filter(keep)
            .collect::<Vec<_>>();
        for shell in loose {
            rows.push(Row::Shell {
                host: self.host.id.clone(),
                shell: shell.clone(),
                depth: 2,
            });
        }
        let loading = node.worktrees.loading() || node.shells.loading();
        if let Some(error) = node.worktrees.error().or(node.shells.error()) {
            rows.push(note(error, true, 2));
        } else if loading {
            rows.push(note("Loading…", false, 2));
        } else if query.is_empty()
            && shells.is_none_or(|shells| shells.is_empty())
            && self
                .orchestrators
                .value()
                .into_iter()
                .flatten()
                .all(|shell| shell.project_id.as_deref() != Some(project.id.as_str()))
        {
            rows.push(note("No shells", false, 2));
        }
    }

    /// Whether anything loaded under this host matches `query`.
    fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let global = self
            .orchestrators
            .value()
            .into_iter()
            .flatten()
            .any(|shell| shell_matches(shell, query));
        let projects = self.projects.value().into_iter().flatten().any(|project| {
            project.name.to_lowercase().contains(query)
                || self
                    .nodes
                    .get(&project.id)
                    .is_some_and(|node| self.project_matches(&project.id, node, query))
        });
        global || projects
    }

    fn project_matches(&self, project: &str, node: &ProjectNode, query: &str) -> bool {
        node.shells
            .value()
            .into_iter()
            .flatten()
            .any(|shell| shell_matches(shell, query))
            || node
                .worktrees
                .value()
                .into_iter()
                .flatten()
                .any(|worktree| worktree.branch.to_lowercase().contains(query))
            || self
                .orchestrators
                .value()
                .into_iter()
                .flatten()
                .filter(|shell| shell.project_id.as_deref() == Some(project))
                .any(|shell| shell_matches(shell, query))
    }
}

fn note(text: &str, error: bool, depth: usize) -> Row {
    Row::Note {
        text: text.to_owned(),
        error,
        depth,
    }
}

fn shell_matches(shell: &RemoteShell, query: &str) -> bool {
    [
        shell.id.as_str(),
        shell.harness.as_deref().unwrap_or("shell"),
        shell.cwd.as_str(),
        if shell.orchestrator {
            "orchestrator"
        } else {
            ""
        },
    ]
    .iter()
    .any(|value| !value.is_empty() && value.to_lowercase().contains(query))
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

/// Parameters of `shell.create` for the project's root.
pub fn shell_create_params(project: &str, kind: NewShellKind) -> Value {
    json!({"project_id": project, "kind": kind.wire()})
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
    })
}

fn worktree(value: &Value) -> Option<RemoteWorktree> {
    Some(RemoteWorktree {
        id: text(value, "id")?,
        branch: text(value, "branch").unwrap_or_default(),
        primary: value
            .get("is_primary")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn session(value: &Value) -> Option<RemoteShell> {
    Some(RemoteShell {
        id: text(value, "id")?,
        project_id: text(value, "project_id"),
        worktree_id: text(value, "worktree_id"),
        orchestrator: text(value, "kind").as_deref() == Some("orchestrator"),
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

    fn shell(id: &str, project: Option<&str>, worktree: Option<&str>) -> RemoteShell {
        RemoteShell {
            id: id.to_owned(),
            project_id: project.map(str::to_owned),
            worktree_id: worktree.map(str::to_owned),
            orchestrator: false,
            harness: None,
            alive: true,
            cwd: "/Users/me/app".to_owned(),
        }
    }

    fn rpc(code: &str, message: &str) -> RemoteError {
        RemoteError::Rpc {
            code: code.to_owned(),
            message: message.to_owned(),
        }
    }

    fn wants_section() -> Wants {
        Wants {
            section: true,
            ..Wants::default()
        }
    }

    /// A tree with one online host called "Studio".
    fn online_tree() -> (RemoteTree, Instant) {
        let now = Instant::now();
        let mut tree = RemoteTree::default();
        tree.set_hosts(vec![host("h1", "Studio")]);
        tree.apply(
            &Request::Status { host: "h1".into() },
            Reply::Status(Ok(status(LinkState::Online))),
        );
        (tree, now)
    }

    /// What a tick of a shown section asks for, leaving out the one-off registry read.
    fn requests(tree: &mut RemoteTree, now: Instant) -> Vec<Request> {
        tree.plan(now, &wants_section())
            .into_iter()
            .filter(|request| *request != Request::Hosts)
            .collect()
    }

    fn answer_lists(tree: &mut RemoteTree) {
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(vec![RemoteProject {
                id: "p1".into(),
                name: "app".into(),
            }])),
        );
        tree.apply(
            &Request::Orchestrators { host: "h1".into() },
            Reply::Orchestrators(Ok(vec![])),
        );
        tree.apply(
            &Request::Worktrees {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Worktrees(Ok(vec![])),
        );
        tree.apply(
            &Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Shells(Ok(vec![])),
        );
    }

    /// An online host, opened on its project "app", with every list answered at `now`.
    fn settled_tree() -> (RemoteTree, Instant) {
        let (mut tree, now) = online_tree();
        tree.toggle_host("h1");
        tree.expand_project("h1", "p1");
        let _ = requests(&mut tree, now);
        answer_lists(&mut tree);
        tree.apply(
            &Request::Status { host: "h1".into() },
            Reply::Status(Ok(status(LinkState::Online))),
        );
        (tree, now)
    }

    fn asks_for_shells(asked: &[Request]) -> bool {
        asked.contains(&Request::Shells {
            host: "h1".into(),
            project: "p1".into(),
        })
    }

    #[test]
    fn nothing_is_asked_while_nobody_is_looking() {
        let mut tree = RemoteTree::default();
        assert!(tree.plan(Instant::now(), &Wants::default()).is_empty());
        tree.set_hosts(vec![host("h1", "Studio")]);
        assert!(tree.plan(Instant::now(), &Wants::default()).is_empty());
    }

    #[test]
    fn a_shown_section_reads_the_registry_then_each_hosts_link_and_repeats_on_a_timer() {
        let now = Instant::now();
        let mut tree = RemoteTree::default();
        let wants = wants_section();
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
        assert!(tree.plan(now, &wants).is_empty());
        tree.apply(
            &Request::Status { host: "h1".into() },
            Reply::Status(Ok(status(LinkState::Online))),
        );
        // Not due yet, then due after the interval.
        assert!(tree.plan(now + Duration::from_secs(1), &wants).is_empty());
        assert_eq!(
            tree.plan(now + STATUS_INTERVAL, &wants),
            [Request::Status { host: "h1".into() }]
        );
        // The registry is read again after its own, longer interval.
        tree.apply(
            &Request::Status { host: "h1".into() },
            Reply::Status(Ok(status(LinkState::Online))),
        );
        assert!(
            tree.plan(now + HOSTS_INTERVAL, &wants)
                .contains(&Request::Hosts)
        );
    }

    #[test]
    fn tabs_alone_watch_their_own_host_and_read_the_registry_once() {
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
        tree.apply(
            &Request::Status { host: "h1".into() },
            Reply::Status(Ok(status(LinkState::Online))),
        );
        let much_later = now + HOSTS_INTERVAL * 3;
        assert_eq!(
            tree.plan(much_later, &wants),
            [Request::Status { host: "h1".into() }]
        );
    }

    #[test]
    fn lists_load_lazily_only_for_what_is_open() {
        let (mut tree, now) = online_tree();
        // Collapsed: only the link is polled.
        assert_eq!(
            requests(&mut tree, now),
            [Request::Status { host: "h1".into() }]
        );

        tree.toggle_host("h1");
        let asked = requests(&mut tree, now + Duration::from_secs(1));
        assert_eq!(
            asked,
            [
                Request::Projects { host: "h1".into() },
                Request::Orchestrators { host: "h1".into() },
            ]
        );
        // A project's own lists wait until it is opened.
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(vec![RemoteProject {
                id: "p1".into(),
                name: "app".into(),
            }])),
        );
        let t = now + Duration::from_secs(2);
        assert!(
            !requests(&mut tree, t).iter().any(|request| matches!(
                request,
                Request::Worktrees { .. } | Request::Shells { .. }
            ))
        );

        tree.toggle_project("h1", "p1");
        let asked = requests(&mut tree, t);
        assert_eq!(
            asked,
            [
                Request::Worktrees {
                    host: "h1".into(),
                    project: "p1".into()
                },
                Request::Shells {
                    host: "h1".into(),
                    project: "p1".into()
                },
            ]
        );
        // Closing the project stops its polling.
        tree.apply(
            &Request::Worktrees {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Worktrees(Ok(vec![])),
        );
        tree.apply(
            &Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Shells(Ok(vec![])),
        );
        tree.toggle_project("h1", "p1");
        let far = t + LIST_INTERVAL * 4;
        assert!(
            !requests(&mut tree, far).iter().any(|request| matches!(
                request,
                Request::Worktrees { .. } | Request::Shells { .. }
            ))
        );
    }

    #[test]
    fn open_lists_are_refreshed_on_a_timer_and_only_while_the_section_shows() {
        let (mut tree, now) = online_tree();
        tree.toggle_host("h1");
        let first = requests(&mut tree, now);
        assert!(first.contains(&Request::Projects { host: "h1".into() }));
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(vec![])),
        );
        tree.apply(
            &Request::Orchestrators { host: "h1".into() },
            Reply::Orchestrators(Ok(vec![])),
        );
        let later = now + LIST_INTERVAL;
        assert!(requests(&mut tree, later).contains(&Request::Projects { host: "h1".into() }));
        // The section is hidden (another tab in front): no list traffic, link polling stops.
        let hidden = later + LIST_INTERVAL * 3;
        assert!(tree.plan(hidden, &Wants::default()).is_empty());
    }

    #[test]
    fn an_offline_host_is_not_asked_for_lists_and_refreshes_them_when_it_returns() {
        let (mut tree, now) = online_tree();
        tree.toggle_host("h1");
        requests(&mut tree, now);
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(vec![RemoteProject {
                id: "p1".into(),
                name: "app".into(),
            }])),
        );
        tree.apply(
            &Request::Orchestrators { host: "h1".into() },
            Reply::Orchestrators(Ok(vec![])),
        );
        tree.apply(
            &Request::Status { host: "h1".into() },
            Reply::Status(Ok(status(LinkState::Offline))),
        );
        let t = now + Duration::from_secs(1);
        let asked = requests(&mut tree, t + LIST_INTERVAL * 2);
        assert!(
            asked
                .iter()
                .all(|request| matches!(request, Request::Status { .. })),
            "{asked:?}"
        );
        // Back online a second later: the lists are asked for again immediately.
        tree.apply(
            &Request::Status { host: "h1".into() },
            Reply::Status(Ok(status(LinkState::Online))),
        );
        let asked = requests(&mut tree, t + LIST_INTERVAL * 2 + Duration::from_secs(1));
        assert!(asked.contains(&Request::Projects { host: "h1".into() }));
    }

    #[test]
    fn an_unreachable_daemon_reads_as_offline_and_a_failed_poll_keeps_the_old_rows() {
        let (mut tree, _) = online_tree();
        tree.toggle_host("h1");
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(vec![RemoteProject {
                id: "p1".into(),
                name: "app".into(),
            }])),
        );
        tree.apply(
            &Request::Orchestrators { host: "h1".into() },
            Reply::Orchestrators(Ok(vec![])),
        );
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Err("No answer in time".into())),
        );
        let rows = tree.rows("");
        assert!(
            rows.iter()
                .any(|row| matches!(row, Row::Project { name, .. } if name == "app"))
        );
        assert!(rows.iter().any(
            |row| matches!(row, Row::Note { text, error: true, .. } if text == "No answer in time")
        ));

        tree.apply(
            &Request::Status { host: "h1".into() },
            Reply::Status(Err("daemon unreachable".into())),
        );
        assert_eq!(tree.link("h1"), Some(Link::Offline));
        assert_eq!(Link::Offline.text(), "Offline (or access revoked)");
        let rows = tree.rows("");
        assert!(matches!(
            rows.as_slice(),
            [Row::Host { link: Some(Link::Offline), .. }, Row::Note { text, .. }]
                if text == "Offline (or access revoked)"
        ));
    }

    #[test]
    fn a_host_with_no_answer_yet_shows_connecting() {
        let mut tree = RemoteTree::default();
        tree.set_hosts(vec![host("h1", "Studio")]);
        tree.toggle_host("h1");
        let rows = tree.rows("");
        assert!(matches!(
            &rows[0],
            Row::Host {
                link: None,
                expanded: true,
                ..
            }
        ));
        assert!(matches!(&rows[1], Row::Note { text, .. } if text == "Connecting…"));
    }

    #[test]
    fn registry_changes_keep_what_was_loaded_for_hosts_that_stay() {
        let (mut tree, _) = online_tree();
        tree.toggle_host("h1");
        assert!(tree.set_hosts(vec![host("h2", "New"), host("h1", "Studio renamed")]));
        assert_eq!(tree.host_label("h1"), Some("Studio renamed"));
        assert_eq!(tree.link("h1"), Some(Link::Online));
        assert!(matches!(&tree.rows("")[1], Row::Host { id, expanded: true, .. } if id == "h1"));
        assert!(!tree.set_hosts(vec![host("h2", "New"), host("h1", "Studio renamed")]));
        assert!(tree.set_hosts(vec![host("h2", "New")]));
        assert_eq!(tree.host_label("h1"), None);
    }

    /// A tree with host h1 open on project p1, which has a worktree, a shell in it and a loose one.
    fn tree_with_shells() -> RemoteTree {
        let (mut tree, now) = online_tree();
        tree.toggle_host("h1");
        tree.expand_project("h1", "p1");
        let _ = requests(&mut tree, now);
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(vec![
                RemoteProject {
                    id: "p1".into(),
                    name: "app".into(),
                },
                RemoteProject {
                    id: "p2".into(),
                    name: "site".into(),
                },
            ])),
        );
        let mut orchestrator = shell("o1", Some("p1"), None);
        orchestrator.orchestrator = true;
        orchestrator.harness = Some("codex".into());
        let mut global = shell("og", None, None);
        global.orchestrator = true;
        tree.apply(
            &Request::Orchestrators { host: "h1".into() },
            Reply::Orchestrators(Ok(vec![global, orchestrator])),
        );
        tree.apply(
            &Request::Worktrees {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Worktrees(Ok(vec![RemoteWorktree {
                id: "w1".into(),
                branch: "main".into(),
                primary: true,
            }])),
        );
        let mut claude = shell("s-claude-0001", Some("p1"), Some("w1"));
        claude.harness = Some("claude".into());
        tree.apply(
            &Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Shells(Ok(vec![claude, shell("s-loose-0002", Some("p1"), None)])),
        );
        tree
    }

    #[test]
    fn rows_nest_host_project_worktree_and_shells_with_orchestrators() {
        let tree = tree_with_shells();
        let rows = tree.rows("");
        let shape = rows
            .iter()
            .map(|row| match row {
                Row::Host { label, .. } => format!("host {label}"),
                Row::Project { name, expanded, .. } => format!("project {name} {expanded}"),
                Row::Worktree { label } => format!("worktree {label}"),
                Row::Shell { shell, depth, .. } => format!("shell{depth} {}", shell.id),
                Row::Chooser { .. } => "chooser".to_owned(),
                Row::Note { text, depth, .. } => format!("note{depth} {text}"),
                Row::Failure { text, depth, .. } => format!("failure{depth} {text}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            shape,
            [
                "host Studio",
                "shell1 og",
                "project app true",
                "shell2 o1",
                "worktree ◆ main",
                "shell3 s-claude-0001",
                "shell2 s-loose-0002",
                "project site false",
            ]
        );
    }

    #[test]
    fn an_empty_open_project_says_so_and_an_unloaded_one_says_loading() {
        let (mut tree, now) = online_tree();
        tree.toggle_host("h1");
        tree.expand_project("h1", "p1");
        let _ = requests(&mut tree, now);
        tree.apply(
            &Request::Projects { host: "h1".into() },
            Reply::Projects(Ok(vec![RemoteProject {
                id: "p1".into(),
                name: "app".into(),
            }])),
        );
        let loading = tree.rows("");
        assert!(
            loading
                .iter()
                .any(|row| matches!(row, Row::Note { text, .. } if text == "Loading…"))
        );
        tree.apply(
            &Request::Orchestrators { host: "h1".into() },
            Reply::Orchestrators(Ok(vec![])),
        );
        for request in [
            Request::Worktrees {
                host: "h1".into(),
                project: "p1".into(),
            },
            Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
        ] {
            let reply = match request {
                Request::Worktrees { .. } => Reply::Worktrees(Ok(vec![])),
                _ => Reply::Shells(Ok(vec![])),
            };
            tree.apply(&request, reply);
        }
        let empty = tree.rows("");
        assert!(
            empty
                .iter()
                .any(|row| matches!(row, Row::Note { text, .. } if text == "No shells"))
        );
    }

    #[test]
    fn search_opens_what_matches_and_hides_the_rest() {
        let tree = tree_with_shells();
        let rows = tree.rows("claude");
        assert!(matches!(&rows[0], Row::Host { .. }));
        assert!(
            rows.iter()
                .any(|row| matches!(row, Row::Shell { shell, .. } if shell.id == "s-claude-0001"))
        );
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row, Row::Shell { shell, .. } if shell.id == "s-loose-0002"))
        );
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row, Row::Project { name, .. } if name == "site"))
        );
        // A host's own name shows all of it; no match hides the host.
        assert!(tree.rows("studio").len() > 3);
        assert!(tree.rows("zzz").is_empty());
    }

    #[test]
    fn a_shell_creation_is_sent_once_and_never_retried_after_it_fails() {
        let (mut tree, now) = settled_tree();
        tree.toggle_chooser("h1", "p1");
        assert!(
            tree.rows("")
                .iter()
                .any(|row| matches!(row, Row::Chooser { .. }))
        );

        assert!(tree.begin_shell("h1", "p1"));
        // The choice closes, and a second click while the first is out sends nothing.
        assert!(
            !tree
                .rows("")
                .iter()
                .any(|row| matches!(row, Row::Chooser { .. }))
        );
        assert!(!tree.begin_shell("h1", "p1"));
        assert_eq!(tree.creation("h1", Some("p1")), Some(&Creation::Creating));

        // The answer never came.
        let t = now + Duration::from_millis(10);
        assert!(
            tree.finish_shell("h1", "p1", Err(RemoteError::Timeout))
                .is_none()
        );
        let Some(Creation::Failed(message)) = tree.creation("h1", Some("p1")).cloned() else {
            panic!("a timed-out creation must stay failed");
        };
        assert!(message.contains("may exist"), "{message}");
        // The lost answer makes the list refresh at once so the person can look.
        assert!(asks_for_shells(&requests(&mut tree, t)));
        // Ticks plan reads only; whatever time passes, the failure stays until a person acts.
        for step in 1..=10 {
            let _ = requests(&mut tree, now + Duration::from_secs(step * 7));
            assert!(matches!(
                tree.creation("h1", Some("p1")),
                Some(Creation::Failed(_))
            ));
        }
        // Only the person can try again.
        assert!(tree.begin_shell("h1", "p1"));
    }

    #[test]
    fn a_refused_creation_shows_the_hosts_reason_and_leaves_the_list_alone() {
        let (mut tree, now) = settled_tree();
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
            tree.creation("h1", Some("p1")),
            Some(&Creation::Failed(
                "codex is not installed or is not on PATH".to_owned()
            ))
        );
        // A definite refusal created nothing, so the list is not refreshed ahead of its timer.
        assert!(!asks_for_shells(&requests(
            &mut tree,
            now + Duration::from_millis(10)
        )));
        // The message is on screen under the project until dismissed.
        assert!(tree.rows("").iter().any(|row| matches!(
            row,
            Row::Failure { text, project: Some(project), .. }
                if text.starts_with("codex") && project == "p1"
        )));
        tree.dismiss_failure("h1", Some("p1"));
        assert_eq!(tree.creation("h1", Some("p1")), Some(&Creation::Idle));
    }

    #[test]
    fn reopening_the_choice_clears_an_old_failure() {
        let (mut tree, _) = settled_tree();
        assert!(tree.begin_shell("h1", "p1"));
        tree.finish_shell("h1", "p1", Err(rpc("cli_error", "tmux is busy")));
        tree.toggle_chooser("h1", "p1");
        assert_eq!(tree.creation("h1", Some("p1")), Some(&Creation::Idle));
    }

    #[test]
    fn a_created_shell_is_handed_back_to_be_opened_and_refreshes_the_list() {
        let (mut tree, now) = settled_tree();
        assert!(tree.begin_shell("h1", "p1"));
        let made = shell("new-shell", Some("p1"), Some("w1"));
        assert_eq!(tree.finish_shell("h1", "p1", Ok(made.clone())), Some(made));
        assert_eq!(tree.creation("h1", Some("p1")), Some(&Creation::Idle));
        // The very next plan lists the project's shells again, ahead of the timer.
        assert!(asks_for_shells(&requests(
            &mut tree,
            now + Duration::from_millis(10)
        )));
    }

    #[test]
    fn project_creation_follows_the_same_rules_per_host() {
        let (mut tree, _) = online_tree();
        assert!(tree.begin_project("h1"));
        assert!(!tree.begin_project("h1"));
        assert!(!tree.begin_project("unknown"));
        let created = RemoteProject {
            id: "p9".into(),
            name: "fresh".into(),
        };
        // "already_exists" is the host's own sentence, shown as is.
        tree.finish_project(
            "h1",
            Err(rpc(
                "already_exists",
                "A project named \"fresh\" already exists on the desktop",
            )),
        );
        assert!(matches!(
            tree.creation("h1", None),
            Some(Creation::Failed(message)) if message.contains("already exists")
        ));
        assert!(tree.begin_project("h1"));
        assert_eq!(
            tree.finish_project("h1", Ok(created.clone())),
            Some(created)
        );
        assert_eq!(tree.creation("h1", None), Some(&Creation::Idle));
        assert!(matches!(
            &tree.rows("")[0],
            Row::Host { expanded: true, .. }
        ));

        // A lost reply: the failure says the project may exist.
        assert!(tree.begin_project("h1"));
        tree.finish_project(
            "h1",
            Err(RemoteError::Protocol(
                "the daemon closed the connection".into(),
            )),
        );
        assert!(matches!(
            tree.creation("h1", None),
            Some(Creation::Failed(message)) if message.contains("may exist")
        ));
    }

    #[test]
    fn a_daemon_that_could_not_be_reached_sent_nothing_so_nothing_may_exist() {
        let (mut tree, now) = settled_tree();
        assert!(tree.begin_shell("h1", "p1"));
        tree.finish_shell(
            "h1",
            "p1",
            Err(RemoteError::Unreachable(
                "Cannot connect to the client daemon for this Mac".into(),
            )),
        );
        assert_eq!(
            tree.creation("h1", Some("p1")),
            Some(&Creation::Failed(
                "Cannot connect to the client daemon for this Mac".to_owned()
            ))
        );
        assert!(!asks_for_shells(&requests(
            &mut tree,
            now + Duration::from_millis(10)
        )));
    }

    #[test]
    fn an_old_host_that_lacks_the_method_is_told_to_update() {
        let (mut tree, _) = online_tree();
        assert!(tree.begin_project("h1"));
        tree.finish_project("h1", Err(rpc("invalid_request", "unsupported RPC method")));
        assert!(matches!(
            tree.creation("h1", None),
            Some(Creation::Failed(message)) if message.contains("Update")
        ));
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
    fn create_requests_use_the_wire_spelling_and_a_project_id_only() {
        for kind in NewShellKind::ALL {
            assert_eq!(
                shell_create_params("p1", kind),
                json!({"project_id": "p1", "kind": kind.wire()})
            );
        }
        let kinds = NewShellKind::ALL.map(NewShellKind::wire);
        assert_eq!(kinds, ["shell", "codex", "claude", "grok"]);
        assert_eq!(project_create_params("App"), json!({"name": "App"}));
    }

    #[test]
    fn replies_parse_leniently_and_report_a_missing_list() {
        let projects = parse_projects(&json!({"projects": [
            {"id": "p1", "name": "app", "root": "/x", "created_at": 1, "extra": true},
            {"name": "no id"},
        ]}))
        .unwrap();
        assert_eq!(
            projects,
            [RemoteProject {
                id: "p1".into(),
                name: "app".into()
            }]
        );
        assert!(parse_projects(&json!({})).is_err());
        let worktrees = parse_worktrees(&json!({"worktrees": [
            {"id": "w1", "project_id": "p1", "branch": "main", "path": "/x", "is_primary": true},
        ]}))
        .unwrap();
        assert_eq!(worktrees[0].branch, "main");
        assert!(worktrees[0].primary);
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
        assert!(orchestrators[0].orchestrator && !orchestrators[0].alive);
        let created =
            parse_created_shell(&json!({"shell_id": "s1", "shell": {"id": "s1", "alive": true}}))
                .unwrap();
        assert_eq!(created.id, "s1");
        assert!(parse_created_shell(&json!({"shell_id": "s1"})).is_err());
        let project = parse_created_project(
            &json!({"project_id": "p1", "project": {"id": "p1", "name": "App"}}),
        )
        .unwrap();
        assert_eq!(project.name, "App");
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
        let tree = tree_with_shells();
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
        let (mut tree, _) = online_tree();
        assert_eq!(tree.strip_for("h1"), None);
        tree.apply(
            &Request::Status { host: "h1".into() },
            Reply::Status(Ok(status(LinkState::Connecting))),
        );
        assert_eq!(
            tree.strip_for("h1").as_deref(),
            Some("RECONNECTING · Studio")
        );
        assert_eq!(tree.strip_for("gone"), None);
    }

    #[test]
    fn answers_for_a_vanished_host_or_the_wrong_request_are_ignored() {
        let (mut tree, _) = online_tree();
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
