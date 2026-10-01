//! Temporary mobile PTY sizing, independent of GPUI. A separate watchdog owns
//! crash recovery; no shell/pane is created, respawned, detached or killed.
//!
//! The watchdog is its own process group (Ctrl-C, a closed terminal or launchd
//! stopping the caller must not take it down) and holds an exclusive lock on
//! `<shell>-watchdog.lock` for as long as it runs. The kernel drops that lock
//! when the process dies, so `resize` can tell a live watchdog from a dead one
//! and starts a new one whenever a lease is written without one.
//!
//! One watchdog per shell covers whichever lease exists. It re-reads the lease
//! under the viewport lock on every check and only releases its own lock, under
//! that same viewport lock, when it is leaving; `resize` checks for a live
//! watchdog under the viewport lock too, so a lease is never left with a
//! watchdog that has already decided to exit.
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const LEASE_MS: u64 = 12_000; // recovery margin within the published 15-second bound
/// A starting watchdog waits this long for a predecessor that is exiting.
const WATCHDOG_CLAIM_WAIT: Duration = Duration::from_secs(2);
/// Consecutive failed checks (tmux busy, a briefly unreadable lease) before the
/// watchdog gives up. Backoff is capped at 1.6 s, so this is about five minutes.
const WATCHDOG_MAX_FAILURES: u32 = 200;
pub type Tmux<'a> = dyn Fn(&[&str]) -> Result<String, String> + 'a;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Size {
    pub shell_id: String,
    pub columns: u32,
    pub rows: u32,
}
#[derive(Serialize, Deserialize)]
struct Lease {
    owner: String,
    connection: String,
    generation: String,
    expires_at: u64,
    baseline: Size,
    policy: String,
    pane_identity: String,
}
pub fn validate_size(columns: u32, rows: u32) -> Result<(), String> {
    if !(20..=300).contains(&columns) || !(8..=160).contains(&rows) {
        return Err("columns must be 20..300 and rows 8..160".into());
    }
    Ok(())
}
fn uuid(id: &str) -> Result<(), String> {
    if Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id) {
        Ok(())
    } else {
        Err("full lowercase UUID required".into())
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn control_dir(home: &Path) -> Result<PathBuf, String> {
    let dir = home.join("terminal-control");
    if !dir.exists() {
        let mut b = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        match b.create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let m = fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err("terminal-control must be a real private directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if m.permissions().mode() & 0o077 != 0 {
            return Err("terminal-control must have mode 700".into());
        }
    }
    Ok(dir)
}
fn open_control(home: &Path, id: &str, kind: &str) -> Result<File, String> {
    uuid(id)?;
    let path = control_dir(home)?.join(format!("{id}-{kind}.lock"));
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("refusing lock symlink".into());
    }
    let mut o = OpenOptions::new();
    o.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    no_follow(&mut o);
    let f = o.open(path).map_err(|e| e.to_string())?;
    private_file(&f)?;
    Ok(f)
}
pub fn lock(home: &Path, id: &str, kind: &str) -> Result<File, String> {
    let f = open_control(home, id, kind)?;
    f.lock_exclusive().map_err(|e| e.to_string())?;
    Ok(f)
}
/// Take the per-shell watchdog lock, waiting briefly for an exiting holder.
/// `None` means another watchdog is alive and will cover the lease.
fn claim_watchdog(home: &Path, id: &str) -> Result<Option<File>, String> {
    let f = open_control(home, id, "watchdog")?;
    let deadline = Instant::now() + WATCHDOG_CLAIM_WAIT;
    loop {
        match f.try_lock_exclusive() {
            Ok(()) => return Ok(Some(f)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}
fn watchdog_alive(home: &Path, id: &str) -> Result<bool, String> {
    let f = open_control(home, id, "watchdog")?;
    match f.try_lock_exclusive() {
        Ok(()) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}
fn spawn_watchdog(
    home: &Path,
    exe: &Path,
    id: &str,
    owner: &str,
    connection: &str,
) -> Result<(), String> {
    let mut command = Command::new(exe);
    command
        .args([
            "shell",
            "viewport-watch",
            id,
            "--owner",
            owner,
            "--lease",
            connection,
        ])
        .env("RIWORK_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
fn path(home: &Path, id: &str) -> Result<PathBuf, String> {
    uuid(id)?;
    Ok(control_dir(home)?.join(format!("{id}-viewport.json")))
}
fn private_file(file: &File) -> Result<(), String> {
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("terminal control file must be regular".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("terminal control file must have mode 600".into());
        }
    }
    Ok(())
}
fn no_follow(options: &mut OpenOptions) {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x100); // Darwin O_NOFOLLOW
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000); // Linux O_NOFOLLOW
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = options;
}
fn read(path: &Path) -> Result<Option<Lease>, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    no_follow(&mut options);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    private_file(&file)?;
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 4096 {
        return Err("viewport file limit".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| e.to_string())
}
fn write(path: &Path, state: &Lease) -> Result<(), String> {
    let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut o = OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&tmp).map_err(|e| e.to_string())?;
        let b = serde_json::to_vec(state).map_err(|e| e.to_string())?;
        f.write_all(&b)
            .and_then(|_| f.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        File::open(path.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
fn geometry(id: &str, t: &Tmux<'_>) -> Result<(Size, String), String> {
    let target = format!("{id}:0.0");
    let s = t(&[
        "display-message",
        "-p",
        "-t",
        &target,
        "#{pane_width}|#{pane_height}|#{window_id}|#{pane_id}|#{window_panes}",
    ])?;
    let f = s.trim().split('|').collect::<Vec<_>>();
    if f.len() != 5 || f[4] != "1" {
        return Err("viewport_unsupported: expected one existing pane in window 0".into());
    }
    Ok((
        Size {
            shell_id: id.into(),
            columns: f[0].parse().map_err(|_| "invalid pane columns")?,
            rows: f[1].parse().map_err(|_| "invalid pane rows")?,
        },
        format!("{}|{}", f[2], f[3]),
    ))
}
fn restore(id: &str, state: &Lease, t: &Tmux<'_>) -> Result<(), String> {
    // Never apply a stale baseline to a replacement window/pane.
    // A desktop split during the lease still needs its window policy restored.
    let target = format!("{id}:0.0");
    if let Ok(identity) = t(&[
        "display-message",
        "-p",
        "-t",
        &target,
        "#{window_id}|#{pane_id}",
    ]) && identity.trim() == state.pane_identity
    {
        let w = format!("{id}:0");
        t(&[
            "resize-window",
            "-t",
            &w,
            "-x",
            &state.baseline.columns.to_string(),
            "-y",
            &state.baseline.rows.to_string(),
        ])?;
        if state.policy.is_empty() {
            t(&["set-option", "-wu", "-t", &w, "window-size"])?;
        } else {
            t(&["set-option", "-w", "-t", &w, "window-size", &state.policy])?;
        }
    }
    Ok(())
}
pub fn resize(
    home: &Path,
    requested: Size,
    owner: &str,
    connection: &str,
    exe: &Path,
    t: &Tmux<'_>,
) -> Result<Size, String> {
    let Size {
        shell_id,
        columns,
        rows,
    } = requested;
    let id = shell_id.as_str();
    uuid(owner)?;
    uuid(connection)?;
    validate_size(columns, rows)?;
    let _lock = lock(home, id, "viewport")?;
    let p = path(home, id)?;
    let old = read(&p)?;
    let mut fresh = false;
    let mut state = match old {
        Some(s) if s.expires_at > now() => {
            if s.owner != owner || s.connection != connection {
                return Err(
                    "viewport_busy: another authenticated connection owns this terminal size"
                        .into(),
                );
            }
            s
        }
        old => {
            if let Some(s) = old {
                restore(id, &s, t)?;
            }
            let (baseline, pane_identity) = geometry(id, t)?;
            let w = format!("{id}:0");
            let policy = t(&["show-options", "-wqv", "-t", &w, "window-size"])?
                .trim()
                .to_owned();
            fresh = true;
            Lease {
                owner: owner.into(),
                connection: connection.into(),
                generation: String::new(),
                expires_at: 0,
                baseline,
                policy,
                pane_identity,
            }
        }
    };
    state.expires_at = now() + LEASE_MS;
    state.generation = Uuid::new_v4().to_string();
    write(&p, &state)?; // record baseline before changing PTY geometry
    // A watchdog that died (or was killed with its caller) never comes back on
    // its own, so every write of the lease makes sure one is alive.
    // If the check itself fails, assume the worst and start one.
    let alive = watchdog_alive(home, id).unwrap_or(false);
    if !alive && let Err(e) = spawn_watchdog(home, exe, id, owner, connection) {
        let _ = restore(id, &state, t);
        let _ = fs::remove_file(&p);
        return Err(format!("viewport watchdog could not start: {e}"));
    }
    let (current, identity) = geometry(id, t)?;
    if identity != state.pane_identity {
        return Err("viewport_unsupported: terminal pane changed".into());
    }
    // Pin the sizing policy even when the first requested cells already match.
    let size = if fresh || current.columns != columns || current.rows != rows {
        let w = format!("{id}:0");
        t(&[
            "resize-window",
            "-t",
            &w,
            "-x",
            &columns.to_string(),
            "-y",
            &rows.to_string(),
        ])?;
        geometry(id, t)?.0
    } else {
        // A renewal of a lease whose cells are still in place (every few
        // seconds, for as long as a phone is attached) has nothing to verify
        // that the call above did not just read.
        current
    };
    if size.columns != columns || size.rows != rows {
        return Err("viewport_unsupported: tmux did not apply requested cells".into());
    }
    Ok(size)
}
pub fn clear(
    home: &Path,
    id: &str,
    owner: &str,
    connection: &str,
    t: &Tmux<'_>,
) -> Result<(), String> {
    uuid(owner)?;
    uuid(connection)?;
    let _lock = lock(home, id, "viewport")?;
    let p = path(home, id)?;
    if let Some(s) = read(&p)? {
        if s.owner != owner || s.connection != connection {
            return Err(
                "viewport_busy: another authenticated connection owns this terminal size".into(),
            );
        }
        restore(id, &s, t)?;
        fs::remove_file(p).map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub fn watch(
    home: &Path,
    id: &str,
    owner: &str,
    connection: &str,
    t: &Tmux<'_>,
) -> Result<(), String> {
    // The arguments identify the lease this process was started for; the
    // watchdog itself covers whatever lease the shell has.
    uuid(owner)?;
    uuid(connection)?;
    watch_with(
        home,
        id,
        t,
        Duration::from_millis(100),
        WATCHDOG_MAX_FAILURES,
    )
}
fn watch_with(
    home: &Path,
    id: &str,
    t: &Tmux<'_>,
    poll: Duration,
    max_failures: u32,
) -> Result<(), String> {
    let p = path(home, id)?;
    let Some(guard) = claim_watchdog(home, id)? else {
        return Ok(()); // a live watchdog already covers this shell
    };
    let mut guard = Some(guard);
    let mut generation = String::new();
    let mut deadline = Instant::now() + Duration::from_millis(LEASE_MS);
    let mut failures = 0u32;
    loop {
        match watch_step(
            home,
            id,
            &p,
            (&mut generation, &mut deadline),
            &mut guard,
            t,
        ) {
            Ok(true) => return Ok(()),
            Ok(false) => failures = 0,
            // The lease file stays until the baseline is restored, so a failed
            // attempt is simply repeated; only a persistent failure ends the
            // watchdog, and the next `resize`, `clear` or expired lease then
            // restores it.
            Err(e) => {
                failures += 1;
                if failures >= max_failures {
                    // Leave the way a normal exit does, so a concurrent
                    // `resize` either sees this watchdog gone or is still
                    // ahead of it.
                    let viewport = lock(home, id, "viewport").ok();
                    drop(guard.take());
                    drop(viewport);
                    return Err(e);
                }
            }
        }
        std::thread::sleep(
            poll + if failures == 0 {
                Duration::ZERO
            } else {
                poll * (1 << failures.min(4))
            },
        );
    }
}
/// One check of the lease. `Ok(true)` ends the watchdog; the watchdog lock is
/// released before the viewport lock so a concurrent `resize` never sees a
/// watchdog that has already decided to leave.
fn watch_step(
    home: &Path,
    id: &str,
    p: &Path,
    (generation, deadline): (&mut String, &mut Instant),
    guard: &mut Option<File>,
    t: &Tmux<'_>,
) -> Result<bool, String> {
    let _lock = lock(home, id, "viewport")?;
    let Some(s) = read(p)? else {
        drop(guard.take());
        return Ok(true);
    };
    if s.generation != *generation {
        *generation = s.generation.clone();
        *deadline = Instant::now() + Duration::from_millis(LEASE_MS);
    }
    if s.expires_at <= now() || Instant::now() >= *deadline {
        restore(id, &s, t)?;
        match fs::remove_file(p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        drop(guard.take());
        return Ok(true);
    }
    Ok(false)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    };

    struct Home(PathBuf);
    impl Home {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("riwork-viewport-test-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The parts of tmux the viewport code touches: one window and one pane.
    struct FakeTmux {
        size: Mutex<(u32, u32)>,
        policy: Mutex<String>,
        resize_failures: AtomicU32,
    }
    impl FakeTmux {
        fn new() -> Self {
            Self {
                size: Mutex::new((80, 24)),
                policy: Mutex::new("smallest".into()),
                resize_failures: AtomicU32::new(0),
            }
        }
        fn size(&self) -> (u32, u32) {
            *self.size.lock().unwrap()
        }
        fn policy(&self) -> String {
            self.policy.lock().unwrap().clone()
        }
        fn run(&self, args: &[&str]) -> Result<String, String> {
            match args {
                ["display-message", "-p", "-t", _, format] if format.contains("pane_width") => {
                    let (columns, rows) = self.size();
                    Ok(format!("{columns}|{rows}|@1|%1|1"))
                }
                ["display-message", "-p", "-t", _, _] => Ok("@1|%1".into()),
                ["show-options", ..] => Ok(self.policy()),
                ["resize-window", "-t", _, "-x", columns, "-y", rows] => {
                    if self.resize_failures.load(Ordering::SeqCst) > 0 {
                        self.resize_failures.fetch_sub(1, Ordering::SeqCst);
                        return Err("server busy".into());
                    }
                    *self.size.lock().unwrap() = (columns.parse().unwrap(), rows.parse().unwrap());
                    Ok(String::new())
                }
                ["set-option", "-w", "-t", _, "window-size", policy] => {
                    *self.policy.lock().unwrap() = (*policy).into();
                    Ok(String::new())
                }
                ["set-option", "-wu", ..] => {
                    self.policy.lock().unwrap().clear();
                    Ok(String::new())
                }
                other => Err(format!("unexpected tmux call {other:?}")),
            }
        }
    }

    fn phone(shell: &str) -> Size {
        Size {
            shell_id: shell.into(),
            columns: 40,
            rows: 30,
        }
    }

    /// A stand-in for the RiWork binary: records each launch and the launched
    /// process's id and process group, then exits (a watchdog that died).
    #[cfg(unix)]
    fn recording_executable(home: &Home) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let log = home.0.join("launches.log");
        let script = home.0.join("fake-riwork");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$$ $(ps -o pgid= -p $$ | tr -d ' ') $*\" >> '{}'\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        (script, log)
    }
    #[cfg(unix)]
    fn launches(log: &Path, at_least: usize) -> Vec<String> {
        for _ in 0..200 {
            let text = fs::read_to_string(log).unwrap_or_default();
            if text.lines().count() >= at_least {
                return text.lines().map(str::to_owned).collect();
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("expected {at_least} watchdog launches");
    }

    #[cfg(unix)]
    #[test]
    fn renewing_a_lease_respawns_a_dead_watchdog_in_its_own_process_group() {
        let home = Home::new();
        let (script, log) = recording_executable(&home);
        let tmux = FakeTmux::new();
        let t = |args: &[&str]| tmux.run(args);
        let (shell, owner, connection) = (
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
        );
        resize(&home.0, phone(&shell), &owner, &connection, &script, &t).unwrap();
        assert_eq!(tmux.size(), (40, 30));
        let first = launches(&log, 1);
        let fields: Vec<_> = first[0].split(' ').collect();
        // The watchdog leads its own process group instead of joining ours.
        assert_eq!(fields[0], fields[1]);
        assert_eq!(&fields[2..5], ["shell", "viewport-watch", shell.as_str()]);
        // It exited right away, so the next renewal of the still-valid lease
        // starts another one.
        resize(&home.0, phone(&shell), &owner, &connection, &script, &t).unwrap();
        launches(&log, 2);
        // A live watchdog (its lock is held) is left alone.
        let alive = claim_watchdog(&home.0, &shell).unwrap().unwrap();
        assert!(watchdog_alive(&home.0, &shell).unwrap());
        resize(&home.0, phone(&shell), &owner, &connection, &script, &t).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(launches(&log, 2).len(), 2);
        drop(alive);
        assert!(!watchdog_alive(&home.0, &shell).unwrap());
        resize(&home.0, phone(&shell), &owner, &connection, &script, &t).unwrap();
        launches(&log, 3);
    }

    #[cfg(unix)]
    #[test]
    fn a_watchdog_that_cannot_start_restores_the_baseline_and_drops_the_lease() {
        let home = Home::new();
        let tmux = FakeTmux::new();
        let t = |args: &[&str]| tmux.run(args);
        let (shell, owner, connection) = (
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
        );
        let error = resize(
            &home.0,
            phone(&shell),
            &owner,
            &connection,
            &home.0.join("missing-binary"),
            &t,
        )
        .unwrap_err();
        assert!(error.contains("watchdog could not start"), "{error}");
        assert_eq!(tmux.size(), (80, 24));
        assert!(read(&path(&home.0, &shell).unwrap()).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn expired_lease_is_restored_despite_transient_tmux_failures() {
        let home = Home::new();
        let (script, _) = recording_executable(&home);
        let tmux = FakeTmux::new();
        let t = |args: &[&str]| tmux.run(args);
        let (shell, owner, connection) = (
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
        );
        resize(&home.0, phone(&shell), &owner, &connection, &script, &t).unwrap();
        assert_eq!(tmux.size(), (40, 30));
        let lease_path = path(&home.0, &shell).unwrap();
        let mut lease = read(&lease_path).unwrap().unwrap();
        lease.expires_at = 1;
        write(&lease_path, &lease).unwrap();
        // The first restore attempts fail, as when tmux is briefly unavailable.
        tmux.resize_failures.store(3, Ordering::SeqCst);
        watch(&home.0, &shell, &owner, &connection, &t).unwrap();
        assert_eq!(tmux.size(), (80, 24));
        assert_eq!(tmux.policy().as_str(), "smallest");
        assert!(read(&lease_path).unwrap().is_none());
        assert!(!watchdog_alive(&home.0, &shell).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_persistently_failing_restore_keeps_the_lease_for_the_next_attempt() {
        let home = Home::new();
        let (script, _) = recording_executable(&home);
        let tmux = FakeTmux::new();
        let t = |args: &[&str]| tmux.run(args);
        let (shell, owner, connection) = (
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
        );
        resize(&home.0, phone(&shell), &owner, &connection, &script, &t).unwrap();
        let lease_path = path(&home.0, &shell).unwrap();
        let mut lease = read(&lease_path).unwrap().unwrap();
        lease.expires_at = 1;
        write(&lease_path, &lease).unwrap();
        tmux.resize_failures.store(u32::MAX, Ordering::SeqCst);
        // Bounded: the watchdog stops after its retry budget, releasing its lock.
        let result = watch_with(&home.0, &shell, &t, Duration::from_millis(1), 5);
        assert!(result.is_err());
        assert!(!watchdog_alive(&home.0, &shell).unwrap());
        // The lease is still there, so a later renewal restores the baseline.
        assert!(read(&lease_path).unwrap().is_some());
        tmux.resize_failures.store(0, Ordering::SeqCst);
        resize(
            &home.0,
            phone(&shell),
            &owner,
            &Uuid::new_v4().to_string(),
            &script,
            &t,
        )
        .unwrap();
        assert_eq!(tmux.size(), (40, 30));
        assert_eq!(read(&lease_path).unwrap().unwrap().baseline.columns, 80);
    }

    #[cfg(unix)]
    #[test]
    fn one_watchdog_covers_a_lease_that_changes_hands() {
        let home = Home::new();
        let (script, log) = recording_executable(&home);
        let tmux = FakeTmux::new();
        let (shell, first_owner, first_connection) = (
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(),
        );
        let t = |args: &[&str]| tmux.run(args);
        resize(
            &home.0,
            phone(&shell),
            &first_owner,
            &first_connection,
            &script,
            &t,
        )
        .unwrap();
        launches(&log, 1);
        let lease_path = path(&home.0, &shell).unwrap();
        let outcome = std::thread::scope(|scope| {
            let watchdog =
                scope.spawn(|| watch_with(&home.0, &shell, &t, Duration::from_millis(5), 5));
            while !watchdog_alive(&home.0, &shell).unwrap() {
                std::thread::sleep(Duration::from_millis(5));
            }
            // Another connection takes the lease over (after the first one's
            // expiry, as `resize` would). The running watchdog must not leave
            // just because the owner differs, and no second one is started.
            let second_owner = Uuid::new_v4().to_string();
            let second_connection = Uuid::new_v4().to_string();
            {
                let _lock = lock(&home.0, &shell, "viewport").unwrap();
                let mut lease = read(&lease_path).unwrap().unwrap();
                lease.owner = second_owner.clone();
                lease.connection = second_connection.clone();
                lease.generation = Uuid::new_v4().to_string();
                write(&lease_path, &lease).unwrap();
            }
            resize(
                &home.0,
                phone(&shell),
                &second_owner,
                &second_connection,
                &script,
                &t,
            )
            .unwrap();
            std::thread::sleep(Duration::from_millis(200));
            assert!(!watchdog.is_finished());
            {
                let _lock = lock(&home.0, &shell, "viewport").unwrap();
                let mut lease = read(&lease_path).unwrap().unwrap();
                lease.expires_at = 1;
                write(&lease_path, &lease).unwrap();
            }
            watchdog.join().unwrap()
        });
        outcome.unwrap();
        assert_eq!(launches(&log, 1).len(), 1);
        assert_eq!(tmux.size(), (80, 24));
        assert!(read(&lease_path).unwrap().is_none());
        assert!(!watchdog_alive(&home.0, &shell).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn only_one_watchdog_per_shell_and_a_successor_waits_for_an_exiting_one() {
        let home = Home::new();
        let shell = Uuid::new_v4().to_string();
        let first = claim_watchdog(&home.0, &shell).unwrap().unwrap();
        let releasing = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(first);
        });
        // A successor started while the old watchdog is leaving takes over.
        let second = claim_watchdog(&home.0, &shell).unwrap();
        releasing.join().unwrap();
        assert!(second.is_some());
        // While it runs, another claim gives up after the wait.
        let started = Instant::now();
        assert!(claim_watchdog(&home.0, &shell).unwrap().is_none());
        assert!(started.elapsed() >= WATCHDOG_CLAIM_WAIT);
    }
}
