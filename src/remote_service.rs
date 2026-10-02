//! Runs the REMOTE section's requests on background threads and holds the state they fill.
//!
//! One `RemoteState` serves every window: the requests a tick plans are deduplicated by the
//! shared `RemoteTree`, so two windows showing the section do not ask twice. The decisions
//! live in `remote_tree`; this module only moves requests to threads and answers back.

use std::{sync::Arc, time::Instant};

use gpui::{App, Global};
use serde_json::json;

use crate::{
    remote_hosts::{Daemons, Host, PairRequest, PairingLink, RemoteCli, RemoteError},
    remote_tree::{
        CREATE_TIMEOUT, LIST_TIMEOUT, NewShellKind, RemoteProject, RemoteShell, RemoteTree, Reply,
        Request, Wants, parse_created_project, parse_created_shell, parse_orchestrators,
        parse_projects, parse_shells, parse_worktrees, project_create_params, shell_create_params,
    },
};

/// The `riwork-remote` binary and the daemons it keeps, shared by every request.
pub struct Backend {
    cli: RemoteCli,
    daemons: Daemons,
}

impl Backend {
    fn new(cli: RemoteCli) -> Self {
        Self {
            daemons: Daemons::new(cli.clone()),
            cli,
        }
    }

    /// Blocking: answer one planned request.
    fn run(&self, request: &Request) -> Reply {
        let list = |host: &str, method: &str, params| {
            self.daemons
                .call(host, method, params, LIST_TIMEOUT)
                .map_err(|error| error.message())
        };
        match request {
            Request::Hosts => Reply::Hosts(self.cli.hosts()),
            Request::Status { host } => {
                Reply::Status(self.daemons.status(host).map_err(|error| error.message()))
            }
            Request::Projects { host } => Reply::Projects(
                list(host, "projects.list", json!({})).and_then(|reply| parse_projects(&reply)),
            ),
            Request::Orchestrators { host } => Reply::Orchestrators(
                list(host, "orchestrators.list", json!({}))
                    .and_then(|reply| parse_orchestrators(&reply)),
            ),
            Request::Worktrees { host, project } => Reply::Worktrees(
                list(host, "worktrees.list", json!({"project_id": project}))
                    .and_then(|reply| parse_worktrees(&reply)),
            ),
            Request::Shells { host, project } => Reply::Shells(
                list(host, "shells.list", json!({"project_id": project}))
                    .and_then(|reply| parse_shells(&reply)),
            ),
        }
    }

    /// Blocking. Sent once; a lost reply is the caller's to explain, never to repeat.
    pub fn create_shell(
        &self,
        host: &str,
        project: &str,
        kind: NewShellKind,
    ) -> Result<RemoteShell, RemoteError> {
        let reply = self.daemons.call(
            host,
            "shell.create",
            shell_create_params(project, kind),
            CREATE_TIMEOUT,
        )?;
        parse_created_shell(&reply).map_err(RemoteError::Protocol)
    }

    /// Blocking. Sent once, like `create_shell`.
    pub fn create_project(&self, host: &str, name: &str) -> Result<RemoteProject, RemoteError> {
        let reply = self.daemons.call(
            host,
            "project.create",
            project_create_params(name),
            CREATE_TIMEOUT,
        )?;
        parse_created_project(&reply).map_err(RemoteError::Protocol)
    }

    /// Blocking: pair a host from a link the user pasted.
    pub fn add_host(&self, link: &str, label: &str) -> Result<(), String> {
        self.cli.add_host(link, label)
    }

    /// Blocking: forget a host. Its daemon notices on its own once nothing asks for it.
    pub fn remove_host(&self, id: &str) -> Result<(), String> {
        self.cli.remove_host(id)?;
        self.daemons.forget(id);
        Ok(())
    }

    /// Blocking: mint a one-time link that lets another Mac control this one.
    pub fn pair_desktop(&self, request: &PairRequest) -> Result<PairingLink, String> {
        self.cli.pair_desktop(request)
    }
}

/// What every window shares about other Macs.
#[derive(Default)]
pub struct RemoteState {
    tree: RemoteTree,
    backend: Option<Arc<Backend>>,
}

impl Global for RemoteState {}

impl RemoteState {
    pub fn tree(&self) -> &RemoteTree {
        &self.tree
    }

    pub fn tree_mut(&mut self) -> &mut RemoteTree {
        &mut self.tree
    }

    /// Found when first needed, and again after a failure, so installing the companion
    /// binary while RiWork runs is enough.
    pub fn backend(&mut self) -> Result<Arc<Backend>, String> {
        if let Some(backend) = &self.backend {
            return Ok(backend.clone());
        }
        let backend = Arc::new(Backend::new(RemoteCli::locate()?));
        self.backend = Some(backend.clone());
        Ok(backend)
    }
}

pub fn init(cx: &mut App) {
    cx.set_global(RemoteState::default());
}

/// The hosts, in registry order, with their link state, for lists outside the panel.
pub fn hosts(cx: &App) -> Vec<(Host, Option<crate::remote_tree::Link>)> {
    cx.global::<RemoteState>()
        .tree()
        .hosts()
        .map(|(host, link)| (host.clone(), link))
        .collect()
}

/// Plan what `wants` calls for and send it. Cheap when nothing is due, so every window can
/// call it on every refresh.
pub fn tick(wants: &Wants, cx: &mut App) {
    let requests = cx
        .global_mut::<RemoteState>()
        .tree_mut()
        .plan(Instant::now(), wants);
    if requests.is_empty() {
        return;
    }
    let backend = cx.global_mut::<RemoteState>().backend();
    match backend {
        Ok(backend) => {
            for request in requests {
                spawn(backend.clone(), request, cx);
            }
        }
        Err(message) => {
            // Nothing can run, so every planned request fails the same way, and the next
            // plan looks for the binary again.
            let mut changed = false;
            for request in requests {
                let reply = failed(&request, message.clone());
                changed |= cx
                    .global_mut::<RemoteState>()
                    .tree_mut()
                    .apply(&request, reply);
            }
            if changed {
                cx.refresh_windows();
            }
        }
    }
}

fn failed(request: &Request, message: String) -> Reply {
    match request {
        Request::Hosts => Reply::Hosts(Err(message)),
        Request::Status { .. } => Reply::Status(Err(message)),
        Request::Projects { .. } => Reply::Projects(Err(message)),
        Request::Orchestrators { .. } => Reply::Orchestrators(Err(message)),
        Request::Worktrees { .. } => Reply::Worktrees(Err(message)),
        Request::Shells { .. } => Reply::Shells(Err(message)),
    }
}

fn spawn(backend: Arc<Backend>, request: Request, cx: &mut App) {
    let asked = request.clone();
    let work = cx
        .background_executor()
        .spawn(async move { backend.run(&asked) });
    cx.spawn(async move |cx| {
        let reply = work.await;
        cx.update(|cx| {
            let changed = cx
                .global_mut::<RemoteState>()
                .tree_mut()
                .apply(&request, reply);
            if changed {
                cx.refresh_windows();
            }
        });
    })
    .detach();
}
