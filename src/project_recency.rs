//! Latest source-file edit times, calculated on the caller's background thread.
//!
//! A project is omitted when its scan is incomplete. A partial maximum would
//! otherwise look like an authoritative last edit, especially for large trees.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};

use crate::store::State;

const SCAN_TIMEOUT: Duration = Duration::from_secs(30);
const PROJECT_TIMEOUT: Duration = Duration::from_secs(5);
const GIT_TIMEOUT: Duration = Duration::from_millis(1500);
const MAX_ENTRIES: usize = 50_000;
const MAX_DEPTH: usize = 40;
const MAX_GIT_OUTPUT: usize = 8 * 1024 * 1024;

/// Return observed UNIX-second mtimes for projects whose source scan completed.
/// Missing roots and projects without regular source files have no entry.
pub fn scan(state: &State) -> BTreeMap<String, u64> {
    let deadline = Instant::now() + SCAN_TIMEOUT;
    let mut results = BTreeMap::new();
    let mut root_cache: BTreeMap<PathBuf, Result<Option<u64>, ()>> = BTreeMap::new();

    for project in &state.projects {
        if Instant::now() >= deadline {
            break;
        }
        let mut roots = BTreeSet::new();
        let mut complete = true;
        let mut budget = Budget {
            deadline: deadline.min(Instant::now() + PROJECT_TIMEOUT),
            remaining_entries: MAX_ENTRIES,
        };
        for path in std::iter::once(&project.root)
            .chain(project.repository_roots.iter())
            .chain(
                state
                    .worktrees
                    .iter()
                    .filter(|worktree| worktree.project_id == project.id)
                    .map(|worktree| &worktree.path),
            )
        {
            if budget.consume().is_err() {
                complete = false;
                break;
            }
            match path.canonicalize() {
                Ok(root) => match fs::symlink_metadata(&root) {
                    Ok(metadata) if metadata.file_type().is_dir() => {
                        roots.insert(root);
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => complete = false,
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => complete = false,
            }
        }
        let mut latest = None;
        for root in roots {
            let result = match root_cache.get(&root) {
                Some(result) => *result,
                None => {
                    let result = scan_root(&root, &mut budget);
                    root_cache.insert(root, result);
                    result
                }
            };
            match result {
                Ok(time) => latest = later(latest, time),
                Err(()) => complete = false,
            }
            if !complete {
                break;
            }
        }
        if complete && let Some(latest) = latest {
            results.insert(project.id.clone(), latest);
        }
    }
    results
}

struct Budget {
    deadline: Instant,
    remaining_entries: usize,
}

impl Budget {
    fn check(&self) -> Result<(), ()> {
        if Instant::now() < self.deadline {
            Ok(())
        } else {
            Err(())
        }
    }

    fn consume(&mut self) -> Result<(), ()> {
        self.check()?;
        self.remaining_entries = self.remaining_entries.checked_sub(1).ok_or(())?;
        Ok(())
    }
}

fn later(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    a.into_iter().chain(b).max()
}

fn excluded_directory(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            ".git"
                | "target"
                | "node_modules"
                | "vendor"
                | "Pods"
                | "DerivedData"
                | ".venv"
                | "venv"
                | ".cache"
                | "__pycache__"
                | ".pytest_cache"
                | ".mypy_cache"
                | ".ruff_cache"
                | ".tox"
                | ".gradle"
                | ".next"
                | ".nuxt"
                | ".svelte-kit"
                | ".parcel-cache"
                | ".turbo"
                | ".Trash"
                | "dist"
                | "build"
                | "coverage"
        )
    )
}

fn ignored_file(name: &OsStr) -> bool {
    matches!(name.to_str(), Some(".DS_Store" | "Thumbs.db"))
}

fn regular_file_time(metadata: &fs::Metadata) -> Result<Option<u64>, ()> {
    if !metadata.file_type().is_file() {
        return Ok(None);
    }
    metadata
        .modified()
        .map_err(|_| ())?
        .duration_since(UNIX_EPOCH)
        .map(|time| Some(time.as_secs()))
        .map_err(|_| ())
}

fn has_git_ancestor(root: &Path) -> bool {
    root.ancestors()
        .any(|path| fs::symlink_metadata(path.join(".git")).is_ok())
}

fn git_files(root: &Path, budget: &Budget) -> Result<Option<Vec<u8>>, ()> {
    budget.check()?;
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // A harness can set these variables for another repository. Never let that
    // redirect the project probe or inject Git configuration into this scan.
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) if !has_git_ancestor(root) => return Ok(None),
        Err(_) => return Err(()),
    };
    let stdout = child.stdout.take().ok_or(())?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout
            .take((MAX_GIT_OUTPUT + 1) as u64)
            .read_to_end(&mut output)
            .map(|_| output);
        let _ = sender.send(result);
    });
    let deadline = budget.deadline.min(Instant::now() + GIT_TIMEOUT);
    let mut output = None;
    loop {
        match receiver.try_recv() {
            Ok(Ok(bytes)) if bytes.len() <= MAX_GIT_OUTPUT => output = Some(bytes),
            Ok(_) | Err(mpsc::TryRecvError::Disconnected) if output.is_none() => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
            _ => {}
        }
        match child.try_wait() {
            Ok(Some(status)) if output.is_some() => {
                if status.success() {
                    return Ok(output);
                }
                return if has_git_ancestor(root) {
                    Err(())
                } else {
                    Ok(None)
                };
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(());
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
fn listed_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn listed_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).as_ref())
}

fn scan_git_files(root: &Path, files: &[u8], budget: &mut Budget) -> Result<Option<u64>, ()> {
    let mut latest = None;
    let mut checked_directories = BTreeSet::new();
    for bytes in files
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
    {
        budget.consume()?;
        let relative = listed_path(bytes);
        if !relative.components().all(|component| match component {
            Component::Normal(name) => !excluded_directory(name),
            _ => false,
        }) || relative.file_name().is_some_and(ignored_file)
        {
            continue;
        }
        // Git does not traverse directory symlinks. Also check parents so a
        // changed path cannot make us inspect files outside the registered root.
        let mut parent = root.to_owned();
        let mut safe = true;
        let components: Vec<_> = relative.components().collect();
        for component in components.iter().take(components.len().saturating_sub(1)) {
            parent.push(component.as_os_str());
            if checked_directories.contains(&parent) {
                continue;
            }
            match fs::symlink_metadata(&parent) {
                Ok(metadata) if metadata.file_type().is_dir() => {
                    checked_directories.insert(parent.clone());
                }
                Ok(_) => {
                    safe = false;
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    safe = false;
                    break;
                }
                Err(_) => return Err(()),
            }
        }
        if !safe {
            continue;
        }
        match fs::symlink_metadata(root.join(relative)) {
            Ok(metadata) => latest = later(latest, regular_file_time(&metadata)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(()),
        }
    }
    Ok(latest)
}

fn scan_root(root: &Path, budget: &mut Budget) -> Result<Option<u64>, ()> {
    match git_files(root, budget)? {
        Some(files) => scan_git_files(root, &files, budget),
        None => scan_plain(root, budget),
    }
}

fn scan_plain(root: &Path, budget: &mut Budget) -> Result<Option<u64>, ()> {
    let mut latest = None;
    let mut directories = vec![(root.to_owned(), 0)];
    while let Some((directory, depth)) = directories.pop() {
        budget.check()?;
        if directory != root && fs::symlink_metadata(directory.join(".git")).is_ok() {
            let files = git_files(&directory, budget)?.ok_or(())?;
            latest = later(latest, scan_git_files(&directory, &files, budget)?);
            continue;
        }
        for entry in fs::read_dir(directory).map_err(|_| ())? {
            budget.consume()?;
            let entry = entry.map_err(|_| ())?;
            let name = entry.file_name();
            if excluded_directory(&name) || ignored_file(&name) {
                continue;
            }
            let file_type = entry.file_type().map_err(|_| ())?;
            if file_type.is_dir() {
                if depth >= MAX_DEPTH {
                    return Err(());
                }
                directories.push((entry.path(), depth + 1));
            } else if file_type.is_file() {
                match fs::symlink_metadata(entry.path()) {
                    Ok(metadata) => latest = later(latest, regular_file_time(&metadata)?),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(()),
                }
            }
        }
    }
    Ok(latest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Project, Worktree};
    use std::fs::{File, FileTimes};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("riwork-recency-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn file(&self, relative: &str, time: u64) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "source").unwrap();
            set_time(&path, time);
            path
        }

        fn state(&self) -> State {
            State {
                projects: vec![Project {
                    id: "project".to_owned(),
                    name: "Project".to_owned(),
                    root: self.0.clone(),
                    repository_roots: Vec::new(),
                    folder_id: None,
                    notify_on_agent_done: false,
                    codex_account: crate::store::ProjectCodexAccount::default(),
                    created_at: 999_999,
                }],
                ..State::default()
            }
        }

        fn git(&self, args: &[&str]) {
            let status = Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn set_time(path: &Path, time: u64) {
        File::open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(time)))
            .unwrap();
    }

    #[test]
    fn uses_file_edits_instead_of_directory_or_registration_times() {
        let fixture = Fixture::new();
        let source = fixture.file("src/main.rs", 200);
        fixture.file("target/generated", 900);
        fixture.file("node_modules/dependency.js", 800);
        fixture.file(".DS_Store", 700);
        let state = fixture.state();
        assert_eq!(scan(&state).get("project"), Some(&200));
        fs::write(&source, "changed source").unwrap();
        set_time(&source, 300);
        assert_eq!(scan(&state).get("project"), Some(&300));
    }

    #[test]
    fn git_uses_tracked_and_nonignored_untracked_files() {
        let fixture = Fixture::new();
        fixture.git(&["init", "--quiet"]);
        let ignore = fixture.file(".gitignore", 10);
        fs::write(&ignore, "generated/\n*.log\n").unwrap();
        set_time(&ignore, 10);
        fixture.file("tracked.rs", 20);
        fixture.git(&["add", ".gitignore", "tracked.rs"]);
        fixture.file("new.rs", 30);
        fixture.file("generated/output.rs", 900);
        fixture.file("debug.log", 800);
        // Even generated files already in the index are excluded.
        fixture.file("target/tracked-build", 700);
        fixture.git(&["add", "-f", "target/tracked-build"]);
        assert_eq!(scan(&fixture.state()).get("project"), Some(&30));
        fs::remove_file(fixture.0.join("tracked.rs")).unwrap();
        assert_eq!(scan(&fixture.state()).get("project"), Some(&30));
    }

    #[test]
    fn project_subdirectory_only_counts_files_inside_its_registered_root() {
        let fixture = Fixture::new();
        fixture.git(&["init", "--quiet"]);
        fixture.file("outside.rs", 900);
        fixture.file("package/source.rs", 20);
        fixture.git(&["add", "outside.rs", "package/source.rs"]);
        fixture.file("package/new.rs", 30);
        let mut state = fixture.state();
        state.projects[0].root = fixture.0.join("package");
        assert_eq!(scan(&state).get("project"), Some(&30));
    }

    #[test]
    fn includes_registered_repository_roots_and_git_worktrees() {
        let repository = Fixture::new();
        repository.git(&["init", "--quiet"]);
        repository.file("source.rs", 20);
        repository.git(&["add", "source.rs"]);
        repository.git(&[
            "-c",
            "user.name=Recency Test",
            "-c",
            "user.email=recency@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "source",
        ]);
        let external = Fixture::new();
        let worktree = external.0.join("worktree");
        repository.git(&[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "recency-test",
            worktree.to_str().unwrap(),
        ]);
        set_time(&worktree.join("source.rs"), 100);
        let folder = Fixture::new();
        folder.file("notes.txt", 10);
        let mut state = folder.state();
        state.projects[0].repository_roots = vec![repository.0.clone()];
        state.worktrees.push(Worktree {
            id: "worktree".to_owned(),
            project_id: "project".to_owned(),
            branch: "recency-test".to_owned(),
            path: worktree,
            is_primary: false,
            repository_root: Some(repository.0.clone()),
            created_at: 999_999,
        });
        assert_eq!(scan(&state).get("project"), Some(&100));
    }

    #[test]
    fn plain_parent_respects_nested_repository_ignores() {
        let fixture = Fixture::new();
        fixture.file("notes.txt", 10);
        let nested = fixture.0.join("repository");
        fs::create_dir(&nested).unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&nested)
                .args(["init", "--quiet"])
                .status()
                .unwrap()
                .success()
        );
        let ignore = fixture.file("repository/.gitignore", 20);
        fs::write(&ignore, "generated/\n").unwrap();
        set_time(&ignore, 20);
        fixture.file("repository/source.rs", 30);
        fixture.file("repository/generated/output.rs", 900);
        assert_eq!(scan(&fixture.state()).get("project"), Some(&30));
    }

    #[test]
    fn missing_and_empty_roots_remain_unknown() {
        let fixture = Fixture::new();
        let mut state = fixture.state();
        assert!(scan(&state).is_empty());
        state.projects[0].root = fixture.0.join("missing");
        assert!(scan(&state).is_empty());
        state.projects[0].root = fixture.file("not-a-directory", 100);
        assert!(scan(&state).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn never_follows_internal_file_or_directory_symlinks() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        fixture.file("source.rs", 20);
        let external = Fixture::new();
        let outside = external.file("outside.rs", 900);
        symlink(&outside, fixture.0.join("linked.rs")).unwrap();
        symlink(&external.0, fixture.0.join("linked-directory")).unwrap();
        symlink(&fixture.0, fixture.0.join("loop")).unwrap();
        assert_eq!(scan(&fixture.state()).get("project"), Some(&20));
        fixture.git(&["init", "--quiet"]);
        fixture.git(&["add", "source.rs", "linked.rs", "linked-directory"]);
        assert_eq!(scan(&fixture.state()).get("project"), Some(&20));
    }

    #[test]
    fn rejects_partial_results_when_entry_budget_runs_out() {
        let fixture = Fixture::new();
        fixture.file("first.rs", 20);
        fixture.file("second.rs", 30);
        let mut budget = Budget {
            deadline: Instant::now() + Duration::from_secs(1),
            remaining_entries: 1,
        };
        assert_eq!(scan_plain(&fixture.0, &mut budget), Err(()));
    }
}
