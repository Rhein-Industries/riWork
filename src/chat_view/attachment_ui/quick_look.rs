//! One Quick Look panel at a time for the composer's attachments. Each panel shows a private
//! copy under the attachment's own name (the host stores it as `content`, without the name's
//! extension) in a fresh directory of one dedicated temporary folder. The copy goes when its
//! panel closes or another replaces it, every one goes when the app quits, and what a run
//! that ended abruptly left there is swept at the next start.

use std::{
    fs, io,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::Child,
    time::{Duration, SystemTime},
};

use uuid::Uuid;

/// A directory in the dedicated folder older than this belongs to no open panel.
pub(super) const STALE: Duration = Duration::from_secs(60 * 60);

/// What `Previews::show` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Shown {
    /// A new panel, the one before it (if any) closed.
    Opened,
    /// The panel for this same file was still open: it is brought forward, not doubled.
    Reused,
}

struct Active {
    child: Child,
    source: PathBuf,
    dir: PathBuf,
}

/// The open panel, if any, and the folder its copies go in.
pub(super) struct Previews {
    root: PathBuf,
    active: Option<Active>,
}

impl Previews {
    pub fn new(root: PathBuf) -> Self {
        Self { root, active: None }
    }

    /// The panel's process while it is open.
    pub fn active_pid(&mut self) -> Option<u32> {
        self.reap();
        self.active.as_ref().map(|active| active.child.id())
    }

    /// Show `source` under `name`: bring its panel forward while it is still open, else close
    /// the open one and `launch` a new one on a fresh copy.
    pub fn show(
        &mut self,
        source: &Path,
        name: &str,
        launch: impl FnOnce(&Path) -> io::Result<Child>,
    ) -> io::Result<Shown> {
        self.reap();
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.source == source)
        {
            return Ok(Shown::Reused);
        }
        self.close();
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)?;
        let dir = self.root.join(Uuid::new_v4().to_string());
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let copy = dir.join(name);
        let child = fs::copy(source, &copy).and_then(|_| launch(&copy));
        match child {
            Ok(child) => {
                self.active = Some(Active {
                    child,
                    source: source.to_owned(),
                    dir,
                });
                Ok(Shown::Opened)
            }
            Err(error) => {
                let _ = fs::remove_dir_all(&dir);
                Err(error)
            }
        }
    }

    /// Forget a panel that has closed, and its copy.
    pub fn reap(&mut self) {
        if let Some(active) = &mut self.active
            && !matches!(active.child.try_wait(), Ok(None))
        {
            let _ = fs::remove_dir_all(&active.dir);
            self.active = None;
        }
    }

    /// Close the open panel and remove its copy.
    pub fn close(&mut self) {
        if let Some(mut active) = self.active.take() {
            let _ = active.child.kill();
            let _ = active.child.wait();
            // Only this freshly allocated directory, never the staged copy.
            let _ = fs::remove_dir_all(&active.dir);
        }
    }
}

/// Remove what is in `root` and was last changed `older_than` before `now`: copies whose
/// panel a quit or crash left behind. How many went.
pub(super) fn sweep(root: &Path, older_than: Duration, now: SystemTime) -> usize {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|changed| now.duration_since(changed).unwrap_or_default() >= older_than)
        })
        .filter(|entry| {
            let path = entry.path();
            if path.is_dir() {
                fs::remove_dir_all(&path).is_ok()
            } else {
                fs::remove_file(&path).is_ok()
            }
        })
        .count()
}

#[cfg(not(test))]
mod app {
    use super::*;
    use std::sync::Mutex;

    static PREVIEWS: Mutex<Option<Previews>> = Mutex::new(None);

    fn root() -> PathBuf {
        // The per-user temporary folder, so another account's panels are never touched.
        std::env::temp_dir().join("riwork-quick-look")
    }

    fn with<T>(f: impl FnOnce(&mut Previews) -> T) -> T {
        let mut previews = PREVIEWS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(previews.get_or_insert_with(|| Previews::new(root())))
    }

    /// Sweep what an earlier run left behind, and close the panel when the app quits.
    pub fn init(cx: &mut gpui::App) {
        std::thread::Builder::new()
            .name("quick-look-sweep".into())
            .spawn(|| sweep(&root(), STALE, SystemTime::now()))
            .ok();
        cx.on_app_quit(|_| {
            with(Previews::close);
            async {}
        })
        .detach();
    }

    /// Show `source` under `name` as `content_type` in the one Quick Look panel.
    pub fn show(source: &Path, name: &str, content_type: &'static str) -> bool {
        use std::process::{Command, Stdio};
        let shown = with(|previews| {
            previews.show(source, name, |copy| {
                Command::new("/usr/bin/qlmanage")
                    .arg("-p")
                    .arg("-c")
                    .arg(content_type)
                    .arg(copy)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            })
        });
        match shown {
            Ok(Shown::Reused) => {
                if let Some(pid) = with(Previews::active_pid) {
                    activate(pid);
                }
                true
            }
            Ok(Shown::Opened) => {
                let pid = with(Previews::active_pid);
                // The copy goes as soon as the panel closes, not only at the next click.
                std::thread::Builder::new()
                    .name("quick-look-reap".into())
                    .spawn(move || {
                        loop {
                            std::thread::sleep(Duration::from_millis(500));
                            let current = with(|previews| {
                                previews.reap();
                                previews.active.as_ref().map(|active| active.child.id())
                            });
                            if current.is_none() || current != pid {
                                break;
                            }
                        }
                    })
                    .ok();
                true
            }
            Err(_) => false,
        }
    }

    /// Bring the panel's process forward.
    fn activate(pid: u32) {
        use objc2::{class, msg_send, runtime::AnyObject};
        let Ok(pid) = i32::try_from(pid) else {
            return;
        };
        // NSApplicationActivateIgnoringOtherApps.
        const IGNORING_OTHER_APPS: usize = 1 << 1;
        unsafe {
            let app: *mut AnyObject = msg_send![
                class!(NSRunningApplication),
                runningApplicationWithProcessIdentifier: pid
            ];
            if !app.is_null() {
                let _: bool = msg_send![app, activateWithOptions: IGNORING_OTHER_APPS];
            }
        }
    }
}

#[cfg(not(test))]
pub(super) use app::{init, show};

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("riwork-ql-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn panel(_: &Path) -> io::Result<Child> {
        Command::new("/bin/sleep").arg("30").spawn()
    }

    fn copies(root: &Path) -> usize {
        fs::read_dir(root).map_or(0, |entries| entries.count())
    }

    #[test]
    fn one_panel_at_a_time_reused_for_the_same_file_replaced_for_another() {
        let base = scratch();
        let (a, b) = (base.join("a"), base.join("b"));
        fs::write(&a, "alpha").unwrap();
        fs::write(&b, "beta").unwrap();
        let root = base.join("ql");
        let mut previews = Previews::new(root.clone());

        assert_eq!(previews.show(&a, "a.txt", panel).unwrap(), Shown::Opened);
        let first = previews.active_pid().unwrap();
        let copy = fs::read_dir(&root).unwrap().next().unwrap().unwrap().path();
        assert_eq!(fs::read_to_string(copy.join("a.txt")).unwrap(), "alpha");
        // The same file again: no second panel, no second copy.
        let launched = std::cell::Cell::new(false);
        let again = previews.show(&a, "a.txt", |_| {
            launched.set(true);
            panel(&a)
        });
        assert_eq!(again.unwrap(), Shown::Reused);
        assert!(!launched.get());
        assert_eq!(previews.active_pid(), Some(first));
        assert_eq!(copies(&root), 1);
        // Another file closes the first panel and its copy.
        assert_eq!(previews.show(&b, "b.txt", panel).unwrap(), Shown::Opened);
        assert_ne!(previews.active_pid(), Some(first));
        assert_eq!(copies(&root), 1);
        // Quitting closes the panel and leaves nothing.
        previews.close();
        assert_eq!(previews.active_pid(), None);
        assert_eq!(copies(&root), 0);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_closed_panel_takes_its_copy_and_a_failed_launch_leaves_none() {
        let base = scratch();
        let source = base.join("s");
        fs::write(&source, "x").unwrap();
        let root = base.join("ql");
        let mut previews = Previews::new(root.clone());
        previews
            .show(&source, "s.txt", |_| Command::new("/usr/bin/true").spawn())
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while previews.active_pid().is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(previews.active_pid(), None);
        assert_eq!(copies(&root), 0);
        let failed = previews.show(&source, "s.txt", |_| Err(io::Error::other("no panel")));
        assert!(failed.is_err());
        assert_eq!(copies(&root), 0);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn the_sweep_removes_only_what_is_stale() {
        let root = scratch();
        let (old, new) = (root.join("old"), root.join("new"));
        fs::create_dir(&old).unwrap();
        fs::write(old.join("a.png"), "x").unwrap();
        fs::create_dir(&new).unwrap();
        let now = SystemTime::now();
        fs::File::open(&old)
            .unwrap()
            .set_modified(now - STALE - Duration::from_secs(60))
            .unwrap();
        assert_eq!(sweep(&root, STALE, now), 1);
        assert!(
            !old.exists() && new.exists(),
            "a panel opened lately keeps its copy"
        );
        assert_eq!(sweep(&root.join("missing"), STALE, now), 0);
        fs::remove_dir_all(root).unwrap();
    }
}
