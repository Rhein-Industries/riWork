//! Passive, one-time import through Orca's public local CLI.
//!
//! The preview freezes both registries. Import rereads Orca, then commits the
//! additions and receipt in one locked RiWork state transaction.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::store::{Project, State, Store, Worktree};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_STDOUT: usize = 32 * 1024 * 1024;
const MAX_STDERR: usize = 256 * 1024;
const REFRESH_MESSAGE: &str =
    "Orca or RiWork changed since this preview. Refresh the preview before importing.";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportReceipt {
    pub source: PathBuf,
    pub project_count: usize,
    pub folder_count: usize,
    pub worktree_count: usize,
    pub completed_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportPreview {
    pub source: PathBuf,
    pub project_count: usize,
    pub folder_count: usize,
    pub worktree_count: usize,
    pub already_imported: Option<ImportReceipt>,
    pub warnings: Vec<String>,
    #[serde(skip)]
    source_snapshot: Option<SourceSnapshot>,
    #[serde(skip)]
    source_identity: Option<ExecutableIdentity>,
    #[serde(skip)]
    destination_snapshot: Vec<u8>,
    #[serde(skip)]
    plan: ImportPlan,
}

#[derive(Clone, Debug)]
pub struct ImportManager {
    store: Store,
    // Tests use an explicit executable, without mutating process environment.
    cli: Option<PathBuf>,
}

impl ImportManager {
    pub fn open_default() -> Result<Self, String> {
        Ok(Self {
            store: Store::open_default()?,
            cli: None,
        })
    }

    pub fn inspect(&self) -> Result<ImportPreview, String> {
        let state = self.store.snapshot()?;
        if let Some(receipt) = state.orca_import {
            return Ok(ImportPreview {
                source: receipt.source.clone(),
                project_count: 0,
                folder_count: 0,
                worktree_count: 0,
                already_imported: Some(receipt),
                warnings: Vec::new(),
                source_snapshot: None,
                source_identity: None,
                destination_snapshot: Vec::new(),
                plan: ImportPlan::default(),
            });
        }
        let source = self.selected_cli()?;
        let identity = executable_identity(&source)?;
        let snapshot = inspect_source(&source)?;
        if executable_identity(&source)? != identity {
            return Err(REFRESH_MESSAGE.to_owned());
        }
        let (plan, mut warnings) = make_plan(&state, &snapshot);
        warnings.splice(0..0, snapshot.warnings.iter().cloned());
        warnings.push("Orca's CLI does not export project folder names; imported projects will be unfiled. Existing RiWork folders are preserved.".to_owned());
        Ok(ImportPreview {
            source,
            project_count: plan.projects.len(),
            folder_count: 0,
            worktree_count: plan.worktrees.len(),
            already_imported: None,
            warnings,
            source_snapshot: Some(snapshot),
            source_identity: Some(identity),
            destination_snapshot: destination_snapshot(&state)?,
            plan,
        })
    }

    pub fn import(&self, preview: &ImportPreview) -> Result<ImportReceipt, String> {
        // Completion is durable even if Orca has subsequently been removed.
        if let Some(receipt) = self.store.snapshot()?.orca_import {
            return Ok(receipt);
        }
        let source = self.selected_cli()?;
        if source != preview.source || preview.already_imported.is_some() {
            return Err(REFRESH_MESSAGE.to_owned());
        }
        let identity = executable_identity(&source)?;
        if preview.source_identity.as_ref() != Some(&identity) {
            return Err(REFRESH_MESSAGE.to_owned());
        }
        let fresh = inspect_source(&source)?;
        if preview.source_snapshot.as_ref() != Some(&fresh)
            || executable_identity(&source)? != identity
        {
            return Err(REFRESH_MESSAGE.to_owned());
        }
        self.store.transaction_if_changed(|state| {
            if let Some(receipt) = &state.orca_import {
                return Ok((receipt.clone(), false));
            }
            if destination_snapshot(state)? != preview.destination_snapshot {
                return Err(REFRESH_MESSAGE.to_owned());
            }
            // A directory can disappear while the three CLI commands run.
            for path in fresh
                .projects
                .iter()
                .flat_map(|project| {
                    std::iter::once(&project.root).chain(project.repository_roots.iter())
                })
                .chain(fresh.worktrees.iter().map(|worktree| &worktree.path))
            {
                if canonical_directory(path).as_ref() != Some(path) {
                    return Err(REFRESH_MESSAGE.to_owned());
                }
            }
            let completed_at = now();
            let mut projects = preview.plan.projects.clone();
            let mut worktrees = preview.plan.worktrees.clone();
            for project in &mut projects {
                project.created_at = completed_at;
            }
            for worktree in &mut worktrees {
                worktree.created_at = completed_at;
            }
            let receipt = ImportReceipt {
                source: source.clone(),
                project_count: projects.len(),
                folder_count: 0,
                worktree_count: worktrees.len(),
                completed_at,
            };
            state.projects.extend(projects);
            state.worktrees.extend(worktrees);
            state.orca_import = Some(receipt.clone());
            // Do not replace active_project_id or any existing metadata.
            Ok((receipt, true))
        })
    }

    fn selected_cli(&self) -> Result<PathBuf, String> {
        match &self.cli {
            Some(path) => resolve_executable(path.as_os_str()),
            None => discover_cli(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ImportPlan {
    projects: Vec<Project>,
    worktrees: Vec<Worktree>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExecutableIdentity {
    length: u64,
    modified: Option<SystemTime>,
}

fn executable_identity(path: &Path) -> Result<ExecutableIdentity, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Cannot inspect Orca CLI {}: {error}", path.display()))?;
    Ok(ExecutableIdentity {
        length: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct SourceSnapshot {
    projects: Vec<CandidateProject>,
    worktrees: Vec<CandidateWorktree>,
    warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct CandidateProject {
    source_id: String,
    name: String,
    root: PathBuf,
    repository_roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct CandidateWorktree {
    source_project_id: String,
    path: PathBuf,
    branch: String,
    is_primary: bool,
    repository_root: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrcaProject {
    id: String,
    display_name: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrcaSetup {
    id: String,
    project_id: String,
    host_id: String,
    #[serde(default)]
    repo_id: Option<String>,
    path: PathBuf,
    kind: String,
    setup_state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrcaWorktree {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    project_host_setup_id: Option<String>,
    #[serde(default)]
    repo_id: Option<String>,
    host_id: String,
    path: PathBuf,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    is_main_worktree: bool,
}

fn inspect_source(cli: &Path) -> Result<SourceSnapshot, String> {
    let project_reply = run_json(cli, &["project", "list", "--json"])?;
    let setups_reply = run_json(cli, &["project", "setups", "--host", "local", "--json"])?;
    let worktree_reply = run_json(cli, &["worktree", "list", "--limit", "10000", "--json"])?;
    let mut projects: Vec<OrcaProject> = decode_array(&project_reply, "projects")?;
    let mut setups: Vec<OrcaSetup> = decode_array(&setups_reply, "setups")?;
    let worktrees: Vec<OrcaWorktree> = decode_array(&worktree_reply, "worktrees")?;
    if worktree_reply.get("truncated").and_then(Value::as_bool) != Some(false) {
        return Err(
            "Orca returned a truncated or incomplete worktree list. Import was not performed."
                .to_owned(),
        );
    }
    let total = worktree_reply
        .get("totalCount")
        .and_then(Value::as_u64)
        .ok_or("Orca worktree response is missing totalCount")?;
    if total != worktrees.len() as u64 {
        return Err(
            "Orca returned an incomplete worktree list. Import was not performed.".to_owned(),
        );
    }
    projects.sort_by(|left, right| left.id.cmp(&right.id));
    setups.sort_by(|left, right| left.path.cmp(&right.path).then(left.id.cmp(&right.id)));
    let mut result = SourceSnapshot {
        projects: Vec::new(),
        worktrees: Vec::new(),
        warnings: Vec::new(),
    };
    let mut accepted_setups = BTreeMap::<String, OrcaSetup>::new();
    let mut known_projects = BTreeSet::new();
    for project in projects {
        if project.id.trim().is_empty()
            || project.display_name.trim().is_empty()
            || !known_projects.insert(project.id.clone())
        {
            return Err("Orca returned duplicate or invalid project identifiers/names".to_owned());
        }
        for setup in setups.iter().filter(|setup| setup.project_id == project.id) {
            if setup.host_id != "local" {
                result.warnings.push(format!(
                    "Skipped remote setup for {}.",
                    project.display_name
                ));
                continue;
            }
            if setup.setup_state != "ready" {
                result.warnings.push(format!(
                    "Skipped setup for {} because it is not ready.",
                    project.display_name
                ));
                continue;
            }
            let Some(root) = canonical_directory(&setup.path) else {
                result.warnings.push(format!(
                    "Skipped missing local project {} ({}).",
                    project.display_name,
                    setup.path.display()
                ));
                continue;
            };
            if setup.id.trim().is_empty() || accepted_setups.contains_key(&setup.id) {
                return Err("Orca returned duplicate or invalid local setup identifiers".to_owned());
            }
            let mut normalized = setup.clone();
            normalized.path = root.clone();
            accepted_setups.insert(setup.id.clone(), normalized);
            result.projects.push(CandidateProject {
                source_id: setup.id.clone(),
                name: project.display_name.trim().to_owned(),
                root: root.clone(),
                repository_roots: if setup.kind == "git" {
                    vec![root]
                } else {
                    Vec::new()
                },
            });
        }
    }
    for worktree in worktrees {
        if worktree.host_id != "local" {
            result.warnings.push(format!(
                "Skipped remote worktree {}.",
                worktree.path.display()
            ));
            continue;
        }
        let setup = if let Some(setup_id) = &worktree.project_host_setup_id {
            accepted_setups.get(setup_id)
        } else {
            let mut matches = accepted_setups.values().filter(|setup| {
                worktree.project_id.as_deref() == Some(&setup.project_id)
                    && worktree.repo_id == setup.repo_id
            });
            let first = matches.next();
            if matches.next().is_some() {
                None
            } else {
                first
            }
        };
        let Some(setup) = setup else {
            result.warnings.push(format!(
                "Skipped worktree {} without a matching ready local project setup.",
                worktree.path.display()
            ));
            continue;
        };
        if worktree
            .project_id
            .as_ref()
            .is_some_and(|id| id != &setup.project_id)
            || worktree.repo_id != setup.repo_id
        {
            result.warnings.push(format!(
                "Skipped worktree {} with mismatched project or repository ownership.",
                worktree.path.display()
            ));
            continue;
        }
        let Some(path) = canonical_directory(&worktree.path) else {
            result.warnings.push(format!(
                "Skipped missing local worktree {}.",
                worktree.path.display()
            ));
            continue;
        };
        result.worktrees.push(CandidateWorktree {
            source_project_id: setup.id.clone(),
            path,
            branch: worktree
                .branch
                .as_deref()
                .unwrap_or("detached")
                .trim()
                .strip_prefix("refs/heads/")
                .unwrap_or(worktree.branch.as_deref().unwrap_or("detached").trim())
                .to_owned(),
            is_primary: worktree.is_main_worktree,
            repository_root: (setup.kind == "git").then(|| setup.path.clone()),
        });
    }
    result.worktrees.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.source_project_id.cmp(&right.source_project_id))
    });
    result.warnings.sort();
    result.warnings.dedup();
    Ok(result)
}

fn make_plan(state: &State, source: &SourceSnapshot) -> (ImportPlan, Vec<String>) {
    let mut plan = ImportPlan::default();
    let mut warnings = Vec::new();
    let mut targets = BTreeMap::<PathBuf, String>::new();
    for project in &state.projects {
        if let Some(root) = canonical_directory(&project.root) {
            targets.entry(root).or_insert_with(|| project.id.clone());
        }
    }
    let mut mapped_projects = BTreeMap::<String, String>::new();
    for project in &source.projects {
        let existing = targets.get(&project.root).cloned();
        let target_id = existing.unwrap_or_else(|| {
            let id = Uuid::new_v4().to_string();
            plan.projects.push(Project {
                id: id.clone(),
                name: project.name.clone(),
                root: project.root.clone(),
                repository_roots: project.repository_roots.clone(),
                folder_id: None,
                notify_on_agent_done: false,
                created_at: 0,
            });
            id
        });
        targets.insert(project.root.clone(), target_id.clone());
        mapped_projects.insert(project.source_id.clone(), target_id);
    }
    let mut paths = BTreeSet::new();
    for worktree in &state.worktrees {
        paths.insert((
            worktree.project_id.clone(),
            canonical_directory(&worktree.path).unwrap_or_else(|| worktree.path.clone()),
        ));
    }
    for worktree in &source.worktrees {
        let Some(project_id) = mapped_projects.get(&worktree.source_project_id) else {
            continue;
        };
        if !paths.insert((project_id.clone(), worktree.path.clone())) {
            continue;
        }
        if worktree.branch.is_empty() {
            warnings.push(format!(
                "Worktree {} has no branch name; imported as detached.",
                worktree.path.display()
            ));
        }
        plan.worktrees.push(Worktree {
            id: Uuid::new_v4().to_string(),
            project_id: project_id.clone(),
            branch: if worktree.branch.is_empty() {
                "detached".to_owned()
            } else {
                worktree.branch.clone()
            },
            path: worktree.path.clone(),
            is_primary: state
                .projects
                .iter()
                .find(|project| &project.id == project_id)
                .and_then(|project| canonical_directory(&project.root))
                .or_else(|| {
                    plan.projects
                        .iter()
                        .find(|project| &project.id == project_id)
                        .map(|project| project.root.clone())
                })
                .as_ref()
                == Some(&worktree.path),
            repository_root: worktree.repository_root.clone(),
            created_at: 0,
        });
    }
    (plan, warnings)
}

fn destination_snapshot(state: &State) -> Result<Vec<u8>, String> {
    let project_paths: Vec<_> = state
        .projects
        .iter()
        .map(|project| canonical_directory(&project.root))
        .collect();
    let worktree_paths: Vec<_> = state
        .worktrees
        .iter()
        .map(|worktree| canonical_directory(&worktree.path))
        .collect();
    serde_json::to_vec(&(
        &state.projects,
        &state.project_folders,
        &state.worktrees,
        project_paths,
        worktree_paths,
    ))
    .map_err(|error| format!("Cannot prepare import preview: {error}"))
}

fn canonical_directory(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || !path.is_dir() {
        return None;
    }
    path.canonicalize().ok()
}

fn decode_array<T: for<'de> Deserialize<'de>>(value: &Value, key: &str) -> Result<Vec<T>, String> {
    let array = value
        .get(key)
        .filter(|value| value.is_array())
        .ok_or_else(|| format!("Orca response is missing the {key} list"))?;
    serde_json::from_value(array.clone())
        .map_err(|error| format!("Cannot decode Orca {key}: {error}"))
}

fn discover_cli() -> Result<PathBuf, String> {
    if let Some(command) = env::var_os("ORCA_CLI_COMMAND") {
        if command.is_empty() {
            return Err(
                "ORCA_CLI_COMMAND is empty. Set it to a single Orca executable path.".to_owned(),
            );
        }
        return resolve_executable(&command);
    }
    let name = if env::var_os("ORCA_DEV_REPO_ROOT").is_some() {
        "orca-dev"
    } else if cfg!(target_os = "linux") {
        "orca-ide"
    } else {
        "orca"
    };
    resolve_executable(std::ffi::OsStr::new(name))
}

fn resolve_executable(command: &std::ffi::OsStr) -> Result<PathBuf, String> {
    let path = Path::new(command);
    if path.components().count() > 1 || path.is_absolute() {
        return executable_path(path).ok_or_else(|| format!("Orca CLI {} is unavailable. Install Orca or set ORCA_CLI_COMMAND to its executable path.", path.display()));
    }
    let mut directories: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    directories.extend([
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
    ]);
    directories.into_iter().find_map(|directory| executable_path(&directory.join(command)))
        .ok_or_else(|| format!("Orca CLI '{}' was not found. Install Orca or set ORCA_CLI_COMMAND to its executable path, then retry.", command.to_string_lossy()))
}

fn executable_path(path: &Path) -> Option<PathBuf> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    path.canonicalize().ok()
}

fn run_json(cli: &Path, arguments: &[&str]) -> Result<Value, String> {
    let mut command = Command::new(cli);
    command
        .args(arguments)
        .env_remove("ORCA_ENVIRONMENT")
        .env_remove("ORCA_PAIRING_CODE")
        .env_remove("ORCA_REMOTE_PAIRING")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot run Orca CLI {}: {error}", cli.display()))?;
    let over_limit = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    spawn_reader(
        child.stdout.take().unwrap(),
        MAX_STDOUT,
        over_limit.clone(),
        sender.clone(),
        false,
    );
    spawn_reader(
        child.stderr.take().unwrap(),
        MAX_STDERR,
        over_limit.clone(),
        sender,
        true,
    );
    let started = Instant::now();
    let status = loop {
        if over_limit.load(Ordering::Relaxed) || started.elapsed() >= COMMAND_TIMEOUT {
            terminate(&mut child);
            return Err(if over_limit.load(Ordering::Relaxed) {
                "Orca CLI output exceeded the import size limit. Import was not performed."
                    .to_owned()
            } else {
                "Orca CLI timed out. Open Orca locally and retry the import preview.".to_owned()
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                terminate(&mut child);
                return Err(format!("Cannot wait for Orca CLI: {error}"));
            }
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    for _ in 0..2 {
        let (is_stderr, bytes) = match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(output) => output,
            Err(_) => {
                terminate(&mut child);
                return Err("Orca CLI did not close its output. Open Orca locally and retry the import preview.".to_owned());
            }
        };
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(error) => {
                terminate(&mut child);
                return Err(format!("Cannot read Orca CLI output: {error}"));
            }
        };
        if is_stderr {
            stderr = bytes;
        } else {
            stdout = bytes;
        }
    }
    if over_limit.load(Ordering::Relaxed) {
        return Err(
            "Orca CLI output exceeded the import size limit. Import was not performed.".to_owned(),
        );
    }
    if !status.success() {
        // Do not print arbitrary CLI stderr: it can contain private project data.
        let _ = stderr;
        return Err(format!(
            "Orca CLI '{}' failed ({status}). Open Orca locally and retry; no import was performed.",
            arguments[..arguments.len().saturating_sub(1)].join(" ")
        ));
    }
    let value: Value = serde_json::from_slice(&stdout)
        .map_err(|error| format!("Orca CLI returned invalid JSON: {error}"))?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(
            "Orca CLI reported a failure. Open Orca locally and retry; no import was performed."
                .to_owned(),
        );
    }
    value
        .get("result")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| "Orca CLI response is missing a valid result object".to_owned())
}

fn spawn_reader(
    mut reader: impl Read + Send + 'static,
    limit: usize,
    over_limit: Arc<AtomicBool>,
    sender: mpsc::Sender<(bool, std::io::Result<Vec<u8>>)>,
    is_stderr: bool,
) {
    thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0u8; 8192];
        let result = loop {
            match reader.read(&mut buffer) {
                Ok(0) => break Ok(output),
                Ok(count) => {
                    let remaining = limit.saturating_sub(output.len());
                    output.extend_from_slice(&buffer[..count.min(remaining)]);
                    if count > remaining {
                        over_limit.store(true, Ordering::Relaxed);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => break Err(error),
            }
        };
        let _ = sender.send((is_stderr, result));
    });
}

fn terminate(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        // The CLI has its own process group, so timeout also closes pipes held
        // by children it started, without touching Orca's existing app process.
        unsafe {
            kill(-(child.id() as i32), 9);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
#[path = "orca_import_tests.rs"]
mod tests;
