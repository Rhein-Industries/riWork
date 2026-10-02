//! Runs the requests `remote_tree` plans on background threads and holds the state they fill.
//!
//! One `RemoteState` serves every window: the requests a tick plans are deduplicated by the
//! shared `RemoteTree`, so two windows showing the same folder do not ask twice. The
//! decisions live in `remote_tree`; this module only moves requests to threads and answers
//! back, and keeps the lists and the selection for the next run (`remote_cache`).

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use gpui::{App, Global};
use serde_json::json;

use crate::{
    paths,
    remote_cache::{self, Kept},
    remote_hosts::{Daemons, Host, PairRequest, PairingLink, RemoteCli, RemoteError},
    remote_tree::{
        CREATE_TIMEOUT, LIST_TIMEOUT, Link, NewShellKind, RemoteProject, RemoteShell, RemoteTree,
        Reply, Request, ShellScope, Wants, parse_created_project, parse_created_shell,
        parse_orchestrators, parse_projects, parse_shells, parse_tasks, parse_worktrees,
        project_create_params, shell_create_params,
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
            Request::Tasks { host, project } => Reply::Tasks(
                list(host, "tasks.list", json!({"project_id": project}))
                    .and_then(|reply| parse_tasks(&reply)),
            ),
            Request::Shells { host, project } => Reply::Shells(
                list(host, "shells.list", json!({"project_id": project}))
                    .and_then(|reply| parse_shells(&reply)),
            ),
        }
    }

    /// Blocking: ask the host for its orchestrators, outside the polling.
    pub fn orchestrators(&self, host: &str) -> Result<Vec<RemoteShell>, String> {
        match self.run(&Request::Orchestrators {
            host: host.to_owned(),
        }) {
            Reply::Orchestrators(result) => result,
            _ => Err("Unexpected reply".to_owned()),
        }
    }

    /// Blocking. Sent once; a lost reply is the caller's to explain, never to repeat.
    pub fn create_shell(
        &self,
        host: &str,
        scope: &ShellScope,
        kind: NewShellKind,
        unrestricted: bool,
    ) -> Result<RemoteShell, RemoteError> {
        let reply = self.daemons.call(
            host,
            "shell.create",
            shell_create_params(scope, kind, unrestricted),
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
    /// The project id of the last window selection on another Mac, for the next launch.
    selected: Option<String>,
    selected_dirty: bool,
    home: Option<PathBuf>,
}

impl Global for RemoteState {}

impl RemoteState {
    pub fn tree(&self) -> &RemoteTree {
        &self.tree
    }

    pub fn tree_mut(&mut self) -> &mut RemoteTree {
        &mut self.tree
    }

    /// The remote project the last window selected, if the selection was not local since.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
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

/// Starts from what the last run kept: the project lists, so a folder shows them at once,
/// and the selection.
pub fn init(cx: &mut App) {
    let mut state = RemoteState::default();
    if let Ok(home) = paths::riwork_home() {
        let kept = remote_cache::load(&home);
        state.tree.keep(kept.hosts);
        state.selected = kept.selected;
        state.home = Some(home);
    }
    cx.set_global(state);
}

/// Record the window's selection for the next launch: a remote project's id, or `None` once a
/// local project is selected.
pub fn select(project: Option<String>, cx: &mut App) {
    let state = cx.global_mut::<RemoteState>();
    if state.selected != project {
        state.selected = project;
        state.selected_dirty = true;
    }
    flush(cx);
}

/// Write down what changed, off the main thread. Writes are numbered so a slow one never
/// replaces a newer one.
fn flush(cx: &mut App) {
    static ISSUED: AtomicU64 = AtomicU64::new(0);
    static WRITTEN: Mutex<u64> = Mutex::new(0);
    let state = cx.global_mut::<RemoteState>();
    let dirty = state.tree.take_kept_dirty() || std::mem::take(&mut state.selected_dirty);
    let Some(home) = state.home.clone().filter(|_| dirty) else {
        return;
    };
    let kept = Kept {
        selected: state.selected.clone(),
        hosts: state.tree.kept(),
    };
    let number = ISSUED.fetch_add(1, Ordering::SeqCst) + 1;
    cx.background_executor()
        .spawn(async move {
            let mut written = WRITTEN.lock().unwrap_or_else(PoisonError::into_inner);
            if number > *written {
                // A cache that cannot be written only costs a dimmed list next time.
                let _ = remote_cache::save(&home, &kept);
                *written = number;
            }
        })
        .detach();
}

/// The hosts, in registry order, with their link state, for lists outside the Projects panel.
pub fn hosts(cx: &App) -> Vec<(Host, Option<Link>)> {
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
        Request::Tasks { .. } => Reply::Tasks(Err(message)),
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
                flush(cx);
            }
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_tree::WorktreeClick;
    use serde_json::Value;
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
        sync::atomic::AtomicUsize,
        thread,
        time::Duration,
    };

    /// A stand-in for a host's client daemon that answers `handler` and records requests.
    struct Daemon {
        directory: PathBuf,
        socket: PathBuf,
        requests: Arc<Mutex<Vec<Value>>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Daemon {
        fn start(handler: impl Fn(&Value, &mut UnixStream) + Send + Sync + 'static) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let name = format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            );
            // Unix socket paths are short on macOS, so the socket sits directly in /tmp.
            let socket = PathBuf::from(format!("/tmp/rs-{name}.sock"));
            let directory = std::env::temp_dir().join(format!("riwork-service-{name}"));
            fs::create_dir_all(&directory).unwrap();
            let listener = UnixListener::bind(&socket).unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let handler = Arc::new(handler);
            let thread = {
                let (requests, stop) = (requests.clone(), stop.clone());
                thread::spawn(move || {
                    for connection in listener.incoming() {
                        if stop.load(Ordering::SeqCst) {
                            break;
                        }
                        let Ok(mut stream) = connection else { continue };
                        let (requests, handler) = (requests.clone(), handler.clone());
                        thread::spawn(move || {
                            let Ok(reader) = stream.try_clone() else {
                                return;
                            };
                            let mut line = String::new();
                            if BufReader::new(reader).read_line(&mut line).is_err() {
                                return;
                            }
                            let Ok(request) = serde_json::from_str::<Value>(&line) else {
                                return;
                            };
                            requests.lock().unwrap().push(request.clone());
                            handler(&request, &mut stream);
                        });
                    }
                })
            };
            Self {
                directory,
                socket,
                requests,
                stop,
                thread: Some(thread),
            }
        }

        /// A `riwork-remote` that answers the two commands the client needs to find the socket.
        fn backend(&self) -> Backend {
            let binary = self.directory.join("riwork-remote");
            let script = format!(
                "#!/bin/sh\ncase \"$1 $2\" in\n  \"client socket\") echo '{}' ;;\nesac\nexit 0\n",
                self.socket.display()
            );
            fs::write(&binary, script).unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
            // The first run of a fresh script is slow on macOS; do it before timing anything.
            let _ = std::process::Command::new(&binary).arg("warm").status();
            Backend::new(RemoteCli::at(binary))
        }

        fn requests(&self) -> Vec<Value> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = UnixStream::connect(&self.socket);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            let _ = fs::remove_file(&self.socket);
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn reply(stream: &mut UnixStream, request: &Value, result: Value) {
        let line = json!({"id": request["id"], "ok": true, "result": result});
        writeln!(stream, "{line}").unwrap();
    }

    const SHELL: &str = r#"{"id":"s9","project_id":"p1","worktree_id":"w1","kind":"project",
        "cwd":"/x","harness":"codex","alive":true,"created_at_unix":1}"#;

    fn created(request: &Value, stream: &mut UnixStream) {
        let shell: Value = serde_json::from_str(SHELL).unwrap();
        reply(stream, request, json!({"shell_id": "s9", "shell": shell}));
    }

    #[test]
    fn a_new_tab_in_a_remote_project_is_one_shell_create_on_that_host() {
        let daemon = Daemon::start(created);
        let backend = daemon.backend();
        let shell = backend
            .create_shell(
                "h1",
                &ShellScope::Project("p1".into()),
                NewShellKind::Codex,
                true,
            )
            .expect("created");
        assert_eq!(shell.id, "s9");
        assert!(shell.alive);
        let requests = daemon.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["op"], "call");
        assert_eq!(requests[0]["method"], "shell.create");
        assert_eq!(
            requests[0]["params"],
            json!({"project_id": "p1", "kind": "codex", "unrestricted": true})
        );
        // Room for an agent to start on the host.
        assert_eq!(requests[0]["timeout_ms"], 90_000);
    }

    #[test]
    fn the_selected_worktree_scopes_the_shell_and_a_plain_shell_is_never_unrestricted() {
        let daemon = Daemon::start(created);
        let backend = daemon.backend();
        backend
            .create_shell(
                "h1",
                &ShellScope::Worktree("w1".into()),
                NewShellKind::Shell,
                true,
            )
            .expect("created");
        assert_eq!(
            daemon.requests()[0]["params"],
            json!({"worktree_id": "w1", "kind": "shell"})
        );
    }

    #[test]
    fn a_creation_whose_reply_is_lost_is_never_sent_again() {
        // The daemon takes the request and hangs up without answering.
        let daemon = Daemon::start(|_, stream| {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        });
        let backend = daemon.backend();
        let error = backend
            .create_shell(
                "h1",
                &ShellScope::Project("p1".into()),
                NewShellKind::Shell,
                false,
            )
            .expect_err("no answer");
        assert!(matches!(error, RemoteError::Protocol(_)), "{error:?}");
        thread::sleep(Duration::from_millis(200));
        assert_eq!(daemon.requests().len(), 1);

        // The same for a project creation.
        let error = backend.create_project("h1", "App").expect_err("no answer");
        assert!(matches!(error, RemoteError::Protocol(_)), "{error:?}");
        thread::sleep(Duration::from_millis(200));
        let methods = daemon
            .requests()
            .iter()
            .map(|request| request["method"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(methods, ["shell.create", "project.create"]);
    }

    /// What a click on a worktree does: choose (`click_worktree`), then ask once.
    /// `Workspace::select_remote_worktree` does exactly this; the host sees what is below.
    fn click_worktree(
        tree: &mut RemoteTree,
        backend: &Backend,
        open_tabs: &[&str],
    ) -> Option<Result<RemoteShell, RemoteError>> {
        match tree.click_worktree("h1", "p1", "w1", open_tabs) {
            WorktreeClick::Start if tree.begin_shell("h1", "p1") => Some(backend.create_shell(
                "h1",
                &ShellScope::Worktree("w1".into()),
                NewShellKind::Shell,
                false,
            )),
            _ => None,
        }
    }

    #[test]
    fn a_click_on_a_worktree_with_no_shell_starts_one_plain_shell_there_exactly_once() {
        let daemon = Daemon::start(created);
        let backend = daemon.backend();
        let mut tree = RemoteTree::default();
        tree.set_hosts(vec![Host {
            id: "h1".into(),
            label: "Studio".into(),
        }]);
        tree.apply(
            &Request::Status { host: "h1".into() },
            Reply::Status(Ok(crate::remote_hosts::HostStatus {
                state: crate::remote_hosts::LinkState::Online,
                rtt_ms: None,
                since: None,
                reason: None,
                label: None,
            })),
        );
        // The host's shells have been read, and none is in the worktree.
        let wanted = Wants {
            selected: Some(crate::remote_tree::SelectedWants {
                host: "h1".into(),
                project: "p1".into(),
                shells: true,
                ..Default::default()
            }),
            ..Wants::default()
        };
        let asked = tree.plan(Instant::now(), &wanted);
        assert!(asked.contains(&Request::Shells {
            host: "h1".into(),
            project: "p1".into()
        }));
        tree.apply(
            &Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Shells(Ok(vec![])),
        );

        // First click: one `shell.create` for the worktree, a plain shell.
        let made = click_worktree(&mut tree, &backend, &[]);
        assert!(matches!(made, Some(Ok(_))), "{made:?}");
        // A second click while that is still pending (its answer not yet recorded) does
        // nothing: no second request reaches the host.
        assert!(click_worktree(&mut tree, &backend, &[]).is_none());
        let requests = daemon.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["method"], "shell.create");
        assert_eq!(
            requests[0]["params"],
            json!({"worktree_id": "w1", "kind": "shell"})
        );

        // The answer arrives: the shell is handed back to be opened.
        let shell = tree
            .finish_shell("h1", "p1", made.unwrap())
            .expect("the new shell");
        assert_eq!(shell.id, "s9");
        // Another click, with its tab open and the list not yet naming it, shows the tab.
        assert!(click_worktree(&mut tree, &backend, &["s9"]).is_none());
        assert_eq!(daemon.requests().len(), 1);
    }

    #[test]
    fn a_click_on_a_worktree_never_retries_a_creation_that_got_no_answer() {
        // The daemon takes the request and hangs up.
        let daemon = Daemon::start(|_, stream| {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        });
        let backend = daemon.backend();
        let mut tree = RemoteTree::default();
        tree.set_hosts(vec![Host {
            id: "h1".into(),
            label: "Studio".into(),
        }]);
        tree.apply(
            &Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Shells(Ok(vec![])),
        );
        // The shells list only exists once asked for, which `begin_shell` also arranges.
        assert!(tree.begin_shell("h1", "p1"));
        tree.finish_shell("h1", "p1", Err(RemoteError::Timeout));
        tree.apply(
            &Request::Shells {
                host: "h1".into(),
                project: "p1".into(),
            },
            Reply::Shells(Ok(vec![])),
        );
        let lost = click_worktree(&mut tree, &backend, &[]);
        assert!(
            matches!(lost, Some(Err(RemoteError::Protocol(_)))),
            "{lost:?}"
        );
        tree.finish_shell("h1", "p1", lost.unwrap());
        thread::sleep(Duration::from_millis(200));
        // Time passes and the person does nothing: nothing more is sent.
        for _ in 0..3 {
            let _ = tree.plan(Instant::now() + Duration::from_secs(60), &Wants::default());
        }
        thread::sleep(Duration::from_millis(200));
        assert_eq!(daemon.requests().len(), 1);
    }

    #[test]
    fn a_new_project_is_one_project_create_with_just_the_name() {
        let daemon = Daemon::start(|request, stream| {
            reply(
                stream,
                request,
                json!({"project_id": "p9", "project": {"id": "p9", "name": "App", "created_at": 3}}),
            );
        });
        let backend = daemon.backend();
        let made = backend.create_project("h1", "App").expect("created");
        assert_eq!((made.id.as_str(), made.name.as_str()), ("p9", "App"));
        let requests = daemon.requests();
        assert_eq!(requests[0]["method"], "project.create");
        assert_eq!(requests[0]["params"], json!({"name": "App"}));
    }

    #[test]
    fn each_list_is_asked_for_by_its_method_with_the_projects_id() {
        let daemon = Daemon::start(|request, stream| {
            let key = match request["method"].as_str().unwrap() {
                "projects.list" => "projects",
                "worktrees.list" => "worktrees",
                "tasks.list" => "tasks",
                "shells.list" => "shells",
                _ => "orchestrators",
            };
            reply(stream, request, json!({ key: [] }));
        });
        let backend = daemon.backend();
        let host = "h1".to_owned();
        let project = "p1".to_owned();
        let asked = [
            Request::Projects { host: host.clone() },
            Request::Orchestrators { host: host.clone() },
            Request::Worktrees {
                host: host.clone(),
                project: project.clone(),
            },
            Request::Tasks {
                host: host.clone(),
                project: project.clone(),
            },
            Request::Shells {
                host: host.clone(),
                project: project.clone(),
            },
        ];
        for request in &asked {
            let reply = backend.run(request);
            let ok = match reply {
                Reply::Projects(result) => result.is_ok(),
                Reply::Orchestrators(result) | Reply::Shells(result) => result.is_ok(),
                Reply::Worktrees(result) => result.is_ok(),
                Reply::Tasks(result) => result.is_ok(),
                Reply::Hosts(_) | Reply::Status(_) => false,
            };
            assert!(ok, "{request:?}");
        }
        let seen = daemon
            .requests()
            .iter()
            .map(|request| {
                (
                    request["method"].as_str().unwrap().to_owned(),
                    request["params"].clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            seen,
            [
                ("projects.list".to_owned(), json!({})),
                ("orchestrators.list".to_owned(), json!({})),
                ("worktrees.list".to_owned(), json!({"project_id": "p1"})),
                ("tasks.list".to_owned(), json!({"project_id": "p1"})),
                ("shells.list".to_owned(), json!({"project_id": "p1"})),
            ]
        );
    }

    #[test]
    fn a_host_that_refuses_is_reported_in_its_own_words() {
        let daemon = Daemon::start(|request, stream| {
            let line = json!({"id": request["id"], "ok": false,
                "error": {"code": "harness_unavailable", "message": "codex is not installed or is not on PATH"}});
            writeln!(stream, "{line}").unwrap();
        });
        let backend = daemon.backend();
        let error = backend
            .create_shell(
                "h1",
                &ShellScope::Project("p1".into()),
                NewShellKind::Codex,
                false,
            )
            .expect_err("refused");
        assert_eq!(error.message(), "codex is not installed or is not on PATH");
    }
}
