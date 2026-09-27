//! Temporary mobile PTY sizing, independent of GPUI. A separate watchdog owns
//! crash recovery; no shell/pane is created, respawned, detached or killed.
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
pub fn lock(home: &Path, id: &str, kind: &str) -> Result<File, String> {
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
    f.lock_exclusive().map_err(|e| e.to_string())?;
    Ok(f)
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
    if fresh {
        let launched = Command::new(exe)
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
            .stderr(Stdio::null())
            .spawn();
        if let Err(e) = launched {
            let _ = restore(id, &state, t);
            let _ = fs::remove_file(&p);
            return Err(format!("viewport watchdog could not start: {e}"));
        }
    }
    let (current, identity) = geometry(id, t)?;
    if identity != state.pane_identity {
        return Err("viewport_unsupported: terminal pane changed".into());
    }
    // Pin the sizing policy even when the first requested cells already match.
    if fresh || current.columns != columns || current.rows != rows {
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
    }
    let (size, _) = geometry(id, t)?;
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
    uuid(owner)?;
    uuid(connection)?;
    let p = path(home, id)?;
    let mut generation = String::new();
    let mut deadline = Instant::now() + Duration::from_millis(LEASE_MS);
    loop {
        {
            let _lock = lock(home, id, "viewport")?;
            let Some(s) = read(&p)? else {
                return Ok(());
            };
            if s.owner != owner || s.connection != connection {
                return Ok(());
            }
            if s.generation != generation {
                generation = s.generation.clone();
                deadline = Instant::now() + Duration::from_millis(LEASE_MS);
            }
            if s.expires_at <= now() || Instant::now() >= deadline {
                restore(id, &s, t)?;
                fs::remove_file(&p).map_err(|e| e.to_string())?;
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
