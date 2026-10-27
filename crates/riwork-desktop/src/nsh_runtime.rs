//! Process ownership and the existing engine reload registry for the AppKit host.
//!
//! All methods that touch the registry belong on the host's runtime worker. The
//! socket listener also runs off the AppKit thread; it only queues open requests.

use fs2::FileExt;
use riwork_engine::runtime::{
    ReloadLaunch, ReloadRequest, RestoreSnapshot, RestoreWatchdog, RuntimeManager,
    RuntimeRegistration, RuntimeWindow,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const FORWARD_TIMEOUT: Duration = Duration::from_secs(5);
const SOCKET_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
type RequestWaker = Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>;

/// The command-line GUI path has already been resolved by the CLI. `None`
/// means reopen/activate the existing desktop, not a CLI subcommand.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRequest {
    pub path: Option<PathBuf>,
}

pub enum InstanceStart {
    Primary(DesktopInstance),
    Replacement(DesktopInstance),
    Forwarded,
}

/// The lock file is never unlinked: replacing its inode would allow two owners.
/// The handoff gate prevents ordinary launches from winning the desktop lock
/// between an old host exiting and its confirmed replacement taking ownership.
pub struct DesktopInstance {
    run_dir: PathBuf,
    lock: Option<File>,
    handoff: Option<File>,
    requests: Option<Receiver<OpenRequest>>,
    waker: RequestWaker,
    stop: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<()>>,
    socket_owned: bool,
}

impl DesktopInstance {
    /// `restore`, if present, must come from `HostRuntime::read_restore`; a
    /// restore ticket is the only way to admit an overlapping GUI process.
    pub fn acquire_or_forward(
        home: &Path,
        request: &OpenRequest,
        restore: Option<&RestoreSnapshot>,
    ) -> Result<InstanceStart, String> {
        let home = canonical_home(home)?;
        let run_dir = home.join("run");
        riwork_core::paths::create_private_dir(&run_dir)
            .map_err(|error| format!("Cannot create desktop runtime directory: {error}"))?;
        // Internal NSH coordination belongs in the Phase 1 nsh/** write area.
        // Keep the frozen desktop.lock/socket paths in run/ unchanged.
        let nsh_dir = home.join("nsh");
        riwork_core::paths::create_private_dir(&nsh_dir)
            .map_err(|error| format!("Cannot create NSH runtime directory: {error}"))?;
        let handoff_path = nsh_dir.join("desktop.handoff.lock");
        let mut instance = Self {
            run_dir,
            lock: None,
            handoff: None,
            requests: None,
            waker: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            listener_thread: None,
            socket_owned: false,
        };

        if let Some(snapshot) = restore {
            if snapshot.state_home != home {
                return Err("Restore ticket belongs to a different RIWORK_HOME".to_owned());
            }
            instance.handoff = try_lock(&handoff_path)?;
            if instance.handoff.is_none() {
                return Err(
                    "Another RiWork replacement already owns the desktop handoff".to_owned(),
                );
            }
            return Ok(InstanceStart::Replacement(instance));
        }

        let deadline = Instant::now() + FORWARD_TIMEOUT;
        loop {
            if let Some(gate) = try_lock(&handoff_path)? {
                if let Some(lock) = try_lock(&instance.run_dir.join("desktop.lock"))? {
                    instance.lock = Some(lock);
                    instance.start_listener()?;
                    drop(gate);
                    return Ok(InstanceStart::Primary(instance));
                }
            }
            match forward(&instance.socket_path(), request) {
                Ok(()) => return Ok(InstanceStart::Forwarded),
                Err(error) if Instant::now() >= deadline => {
                    return Err(format!(
                        "RiWork is already starting or running, but its desktop socket did not accept the open request: {error}"
                    ));
                }
                Err(_) => thread::sleep(Duration::from_millis(25)),
            }
        }
    }

    pub fn drain_requests(&self) -> Vec<OpenRequest> {
        self.requests
            .as_ref()
            .map(|requests| requests.try_iter().collect())
            .unwrap_or_default()
    }

    /// Called on the socket worker after an accepted request is queued. Keep
    /// the callback short (for example, wake the host's runtime worker).
    pub fn set_request_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.waker.lock().unwrap_or_else(|error| error.into_inner()) = Some(Box::new(wake));
    }

    /// Stop accepting new opens when shutdown starts, retaining desktop.lock
    /// until the host has actually closed its windows. A concurrent launch
    /// retries instead of receiving an acknowledgement that would be lost.
    pub fn stop_forwarding(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.listener_thread.take() {
            // Wake blocking accept without an idle polling timer.
            let _ = UnixStream::connect(self.socket_path());
            let _ = worker.join();
        }
        // A replacement waiting on its predecessor must never remove that
        // predecessor's socket. Remove ours while still holding desktop.lock.
        if self.socket_owned {
            let _ = fs::remove_file(self.socket_path());
            self.socket_owned = false;
        }
    }

    /// Call only after `mark_restore_ready` has confirmed every restored
    /// window. The old host still owns desktop.lock until it observes that
    /// confirmation and exits, so callers retry on their worker's normal tick.
    pub fn try_complete_handoff(&mut self) -> Result<bool, String> {
        if self.lock.is_some() {
            return Ok(true);
        }
        if self.handoff.is_none() {
            return Err("Desktop instance has no replacement handoff".to_owned());
        }
        let Some(lock) = try_lock(&self.run_dir.join("desktop.lock"))? else {
            return Ok(false);
        };
        self.lock = Some(lock);
        if let Err(error) = self.start_listener() {
            if self.socket_owned {
                let _ = fs::remove_file(self.socket_path());
                self.socket_owned = false;
            }
            self.lock = None;
            return Err(error);
        }
        self.handoff = None;
        Ok(true)
    }

    fn socket_path(&self) -> PathBuf {
        self.run_dir.join("desktop.sock")
    }

    fn start_listener(&mut self) -> Result<(), String> {
        let socket = self.socket_path();
        match fs::symlink_metadata(&socket) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                fs::remove_file(&socket)
                    .map_err(|error| format!("Cannot remove stale desktop socket: {error}"))?;
            }
            Ok(_) => {
                return Err(format!(
                    "Desktop socket path is not a socket: {}",
                    socket.display()
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot inspect desktop socket: {error}")),
        }
        let listener = UnixListener::bind(&socket)
            .map_err(|error| format!("Cannot bind desktop socket {}: {error}", socket.display()))?;
        self.socket_owned = true;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("Cannot protect desktop socket: {error}"))?;
        let (send, receive) = mpsc::sync_channel(64);
        let stop = self.stop.clone();
        let waker = self.waker.clone();
        self.listener_thread = Some(
            thread::Builder::new()
                .name("riwork-desktop-open".to_owned())
                .spawn(move || {
                    for incoming in listener.incoming() {
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        let Ok(mut stream) = incoming else { break };
                        let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
                        let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
                        let result = read_request(&stream).and_then(|request| {
                            send.try_send(request)
                                .map_err(|_| "Desktop open queue is full or closed".to_owned())
                        });
                        if result.is_ok() {
                            if let Some(wake) = waker
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .as_ref()
                            {
                                wake();
                            }
                        }
                        let reply = if result.is_ok() {
                            b"ok\n".as_slice()
                        } else {
                            b"error\n".as_slice()
                        };
                        let _ = stream.write_all(reply);
                    }
                })
                .map_err(|error| format!("Cannot start desktop socket listener: {error}"))?,
        );
        self.requests = Some(receive);
        Ok(())
    }
}

impl Drop for DesktopInstance {
    fn drop(&mut self) {
        self.stop_forwarding();
    }
}

fn canonical_home(home: &Path) -> Result<PathBuf, String> {
    riwork_core::paths::create_private_dir(home)
        .map_err(|error| format!("Cannot create RIWORK_HOME: {error}"))?;
    home.canonicalize()
        .map_err(|error| format!("Cannot resolve RIWORK_HOME: {error}"))
}

fn try_lock(path: &Path) -> Result<Option<File>, String> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("Cannot open desktop lock {}: {error}", path.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(format!("Cannot lock desktop: {error}")),
    }
}

fn read_request(stream: &UnixStream) -> Result<OpenRequest, String> {
    let mut bytes = Vec::new();
    BufReader::new(stream.take(MAX_REQUEST_BYTES + 1))
        .read_until(b'\n', &mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES || bytes.last() != Some(&b'\n') {
        return Err("Invalid or oversized desktop open request".to_owned());
    }
    serde_json::from_slice(&bytes).map_err(|error| format!("Invalid desktop open request: {error}"))
}

fn forward(socket: &Path, request: &OpenRequest) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err("Desktop open request is too large".to_owned());
    }
    let mut stream = UnixStream::connect(socket).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(SOCKET_TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(SOCKET_TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    let mut response = [0; 3];
    stream
        .read_exact(&mut response)
        .map_err(|error| error.to_string())?;
    if &response != b"ok\n" {
        return Err("Desktop refused the open request".to_owned());
    }
    Ok(())
}

/// The host supplies Rust-owned window/layout snapshots. JS only posts the
/// restore-ready acknowledgement; it is never queried for persisted state.
pub struct HostRuntime {
    manager: RuntimeManager,
    registration: RuntimeRegistration,
    restore: Option<RestoreSnapshot>,
    barrier: RestoreBarrier,
    watchdog: Option<RestoreWatchdog>,
}

#[derive(Default)]
struct RestoreBarrier {
    expected: Option<BTreeSet<String>>,
    ready: BTreeSet<String>,
}

impl RestoreBarrier {
    fn set_expected(
        &mut self,
        ids: impl IntoIterator<Item = String>,
        count: usize,
    ) -> Result<(), String> {
        if self.expected.is_some() {
            return Err("Restore window IDs have already been registered".to_owned());
        }
        let ids: Vec<_> = ids.into_iter().collect();
        let expected: BTreeSet<_> = ids.iter().cloned().collect();
        if ids.len() != count || expected.len() != count {
            return Err("Restore window IDs do not match the reload snapshot".to_owned());
        }
        self.expected = Some(expected);
        Ok(())
    }

    fn mark_ready(&mut self, id: &str) -> Result<bool, String> {
        let expected = self
            .expected
            .as_ref()
            .ok_or("Restore window IDs have not been registered")?;
        if !expected.contains(id) {
            return Err("Restore acknowledgement belongs to an unknown window".to_owned());
        }
        self.ready.insert(id.to_owned());
        Ok(&self.ready == expected)
    }
}

impl HostRuntime {
    /// Read and validate the engine's ticket before considering any overlap.
    /// RIWORK_HOME remains fixed by the process; a ticket cannot change it.
    pub fn read_restore(home: &Path) -> Result<Option<RestoreSnapshot>, String> {
        let home = canonical_home(home)?;
        let snapshot = RuntimeManager::open_default()?.restore_from_env()?;
        if snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.state_home != home)
        {
            return Err("Restore ticket belongs to a different RIWORK_HOME".to_owned());
        }
        Ok(snapshot)
    }

    pub fn register(home: &Path, restore: Option<RestoreSnapshot>) -> Result<Self, String> {
        Self::register_with_manager(RuntimeManager::open_default()?, home, restore)
    }

    fn register_with_manager(
        manager: RuntimeManager,
        home: &Path,
        restore: Option<RestoreSnapshot>,
    ) -> Result<Self, String> {
        let home = canonical_home(home)?;
        if restore
            .as_ref()
            .is_some_and(|snapshot| snapshot.state_home != home)
        {
            return Err("Restore ticket belongs to a different RIWORK_HOME".to_owned());
        }
        let registration = manager.register(home)?;
        let watchdog = restore
            .as_ref()
            .map(|snapshot| registration.start_restore_watchdog(snapshot));
        Ok(Self {
            manager,
            registration,
            restore,
            barrier: RestoreBarrier::default(),
            watchdog,
        })
    }

    pub fn set_restore_window_ids(
        &mut self,
        ids: impl IntoIterator<Item = String>,
    ) -> Result<(), String> {
        let Some(snapshot) = &self.restore else {
            return Ok(());
        };
        self.barrier.set_expected(ids, snapshot.windows.len())
    }

    pub fn mark_restore_ready(
        &mut self,
        window_id: &str,
        windows: Vec<RuntimeWindow>,
    ) -> Result<bool, String> {
        let Some(snapshot) = &self.restore else {
            return Ok(false);
        };
        if !self.barrier.mark_ready(window_id)? {
            return Ok(false);
        }
        if !same_window_targets(&windows, &snapshot.windows) {
            return Err("Restored windows do not match the reload snapshot".to_owned());
        }
        if self
            .watchdog
            .as_ref()
            .is_some_and(|watchdog| !watchdog.begin_confirm())
        {
            return Err("Restore watchdog has already stopped this replacement".to_owned());
        }
        let result = self.registration.publish_windows(windows).and_then(|_| {
            self.manager
                .mark_restore_ready(snapshot, &self.registration)
        });
        if let Err(error) = result {
            if let Some(watchdog) = &self.watchdog {
                watchdog.abort_confirm();
            }
            return Err(error);
        }
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.finish_confirm();
        }
        self.restore = None;
        Ok(true)
    }

    pub fn publish(&mut self, windows: Vec<RuntimeWindow>) -> Result<(), String> {
        self.registration.publish_windows(windows)
    }

    /// The engine writes only changes or its 30-second heartbeat. A busy
    /// registry returns false, so the next host worker tick can retry.
    pub fn try_publish(&mut self, windows: Vec<RuntimeWindow>) -> Result<bool, String> {
        self.registration.try_publish_windows(windows)
    }

    pub fn pending_reload(&self) -> Result<Option<ReloadRequest>, String> {
        self.registration.pending_reload()
    }

    /// The caller must flush Rust draft/layout services before invoking this.
    #[allow(dead_code)] // Phase 1b keeps restart unavailable until P1a supplies these flush hooks.
    pub fn launch_reload(
        &self,
        request: &ReloadRequest,
        windows: Vec<RuntimeWindow>,
    ) -> Result<ReloadLaunch, String> {
        self.registration.launch_reload(request, windows)
    }

    #[allow(dead_code)] // Paired with launch_reload once native persistence is connected.
    pub fn reload_ready(&self, launch: &ReloadLaunch) -> Result<bool, String> {
        self.registration.reload_ready(launch)
    }

    pub fn fail_reload(&self, request: &ReloadRequest, message: &str) -> Result<(), String> {
        self.registration.fail_reload(request, message)
    }
}

fn same_window_targets(actual: &[RuntimeWindow], expected: &[RuntimeWindow]) -> bool {
    if actual.len() != expected.len() {
        return false;
    }
    let mut remaining: Vec<_> = expected.iter().collect();
    for window in actual {
        let Some(index) = remaining.iter().position(|other| {
            window.kind == other.kind
                && window.project_id == other.project_id
                && window.path == other.path
        }) else {
            return false;
        };
        remaining.remove(index);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use riwork_engine::runtime::{WindowKind, WindowMode};
    use uuid::Uuid;

    struct Fixture {
        root: PathBuf,
        home: PathBuf,
        runtime: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            // macOS has a short Unix-socket path limit; /tmp also keeps this
            // independent from the user's RIWORK_HOME and runtime registry.
            let root = PathBuf::from("/tmp").join(format!("rwnsh-{}", Uuid::new_v4().simple()));
            let home = root.join("home");
            let runtime = root.join("runtime");
            fs::create_dir_all(&home).unwrap();
            Self {
                root,
                home,
                runtime,
            }
        }
        fn primary(&self) -> DesktopInstance {
            match DesktopInstance::acquire_or_forward(&self.home, &OpenRequest::default(), None)
                .unwrap()
            {
                InstanceStart::Primary(instance) => instance,
                _ => panic!("fixture must start a primary"),
            }
        }
        fn window(&self, project: &str) -> RuntimeWindow {
            RuntimeWindow {
                project_id: Some(project.to_owned()),
                path: self.root.join(project),
                bounds: None,
                mode: WindowMode::Windowed,
                layout: None,
                focus_mode: false,
                focus_centered: false,
                kind: WindowKind::Project,
                global: None,
            }
        }
        fn snapshot(&self) -> RestoreSnapshot {
            RestoreSnapshot {
                ticket_id: Uuid::new_v4().to_string(),
                request_id: Uuid::new_v4().to_string(),
                old_instance_id: Uuid::new_v4().to_string(),
                replacement_executable: std::env::current_exe().unwrap(),
                state_home: self.home.canonicalize().unwrap(),
                windows: vec![self.window("one")],
                created_at: 0,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn forwards_path_and_reopen_without_creating_a_second_primary() {
        let fixture = Fixture::new();
        let mut primary = fixture.primary();
        let (wake_send, wake_receive) = mpsc::channel();
        primary.set_request_waker(move || {
            let _ = wake_send.send(());
        });
        let request = OpenRequest {
            path: Some(fixture.root.join("project with spaces")),
        };
        for open in [&request, &OpenRequest::default()] {
            assert!(matches!(
                DesktopInstance::acquire_or_forward(&fixture.home, open, None).unwrap(),
                InstanceStart::Forwarded
            ));
            wake_receive.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(primary.drain_requests(), vec![open.clone()]);
        }
        assert!(
            try_lock(&fixture.home.join("run/desktop.lock"))
                .unwrap()
                .is_none()
        );
        primary.stop_forwarding();
        assert!(!fixture.home.join("run/desktop.sock").exists());
        assert!(
            try_lock(&fixture.home.join("run/desktop.lock"))
                .unwrap()
                .is_none()
        );
        drop(primary);
        assert!(!fixture.home.join("run/desktop.sock").exists());
        assert!(matches!(
            DesktopInstance::acquire_or_forward(&fixture.home, &request, None).unwrap(),
            InstanceStart::Primary(_)
        ));
    }

    #[test]
    fn replacement_keeps_old_socket_and_prevents_handoff_race() {
        let fixture = Fixture::new();
        let primary = fixture.primary();
        assert!(fixture.home.join("nsh/desktop.handoff.lock").is_file());
        assert!(!fixture.home.join("run/desktop.handoff.lock").exists());
        let snapshot = fixture.snapshot();
        let InstanceStart::Replacement(mut replacement) = DesktopInstance::acquire_or_forward(
            &fixture.home,
            &OpenRequest::default(),
            Some(&snapshot),
        )
        .unwrap() else {
            panic!("expected replacement")
        };
        assert!(!replacement.try_complete_handoff().unwrap());
        assert!(
            try_lock(&fixture.home.join("nsh/desktop.handoff.lock"))
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            DesktopInstance::acquire_or_forward(&fixture.home, &OpenRequest::default(), None)
                .unwrap(),
            InstanceStart::Forwarded
        ));
        assert_eq!(primary.drain_requests(), vec![OpenRequest::default()]);
        drop(primary);
        assert!(replacement.try_complete_handoff().unwrap());
        assert!(
            try_lock(&fixture.home.join("nsh/desktop.handoff.lock"))
                .unwrap()
                .is_some()
        );
        assert!(matches!(
            DesktopInstance::acquire_or_forward(&fixture.home, &OpenRequest::default(), None)
                .unwrap(),
            InstanceStart::Forwarded
        ));
        assert_eq!(replacement.drain_requests(), vec![OpenRequest::default()]);
    }

    #[test]
    fn abandoned_replacement_does_not_remove_running_socket() {
        let fixture = Fixture::new();
        let primary = fixture.primary();
        let snapshot = fixture.snapshot();
        let replacement = DesktopInstance::acquire_or_forward(
            &fixture.home,
            &OpenRequest::default(),
            Some(&snapshot),
        )
        .unwrap();
        drop(replacement);
        assert!(fixture.home.join("run/desktop.sock").exists());
        forward(&primary.socket_path(), &OpenRequest::default()).unwrap();
    }

    #[test]
    fn reclaims_stale_socket_but_never_deletes_a_regular_file() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.home.join("run")).unwrap();
        let path = fixture.home.join("run/desktop.sock");
        drop(UnixListener::bind(&path).unwrap());
        drop(fixture.primary());
        fs::write(&path, "keep me").unwrap();
        assert!(
            DesktopInstance::acquire_or_forward(&fixture.home, &OpenRequest::default(), None)
                .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep me");
    }

    #[test]
    fn restore_acknowledgements_are_per_window_and_idempotent() {
        let mut barrier = RestoreBarrier::default();
        assert!(barrier.mark_ready("one").is_err());
        assert!(
            barrier
                .set_expected(vec!["one".into(), "one".into()], 2)
                .is_err()
        );
        barrier
            .set_expected(vec!["one".into(), "two".into()], 2)
            .unwrap();
        assert!(!barrier.mark_ready("one").unwrap());
        assert!(!barrier.mark_ready("one").unwrap());
        assert!(barrier.mark_ready("other").is_err());
        assert!(barrier.mark_ready("two").unwrap());
    }

    #[test]
    fn restore_matches_targets_even_if_window_order_changes() {
        let fixture = Fixture::new();
        let one = fixture.window("one");
        let two = fixture.window("two");
        assert!(same_window_targets(
            &[one.clone(), two.clone()],
            &[two.clone(), one.clone()]
        ));
        assert!(!same_window_targets(
            &[one.clone(), one.clone()],
            &[one.clone(), two]
        ));
        assert!(!same_window_targets(&[], &[one]));
    }

    #[test]
    fn publishes_and_unregisters_only_in_explicit_fixture_registry() {
        let fixture = Fixture::new();
        let manager = RuntimeManager::at(fixture.runtime.clone()).unwrap();
        let mut runtime =
            HostRuntime::register_with_manager(manager.clone(), &fixture.home, None).unwrap();
        let windows = vec![fixture.window("one"), fixture.window("two")];
        runtime
            .set_restore_window_ids(["ordinary-one".into(), "ordinary-two".into()])
            .unwrap();
        runtime.publish(windows.clone()).unwrap();
        assert!(runtime.try_publish(windows.clone()).unwrap());
        let instances = manager.instances().unwrap();
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].windows, windows);
        assert_eq!(
            instances[0].state_home,
            fixture.home.canonicalize().unwrap()
        );
        assert!(
            !runtime
                .mark_restore_ready("ordinary-window", windows)
                .unwrap()
        );
        drop(runtime);
        assert!(manager.instances().unwrap().is_empty());
    }
}
