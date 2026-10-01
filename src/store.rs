//! Durable projects, Git worktrees, and tasks shared by the UI and `riwork` CLI.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    env, fs,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    #[serde(default)]
    pub repository_roots: Vec<PathBuf>,
    #[serde(default)]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub notify_on_agent_done: bool,
    #[serde(default)]
    pub codex_account: ProjectCodexAccount,
    pub created_at: u64,
}

/// The app preference remains the default for older projects. SystemDefault
/// deliberately differs from Inherit when the app selects a saved account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", content = "account_id", rename_all = "snake_case")]
pub enum ProjectCodexAccount {
    #[default]
    Inherit,
    SystemDefault,
    Saved(String),
}

/// An organizational group in RiWork, independent of directories on disk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectFolder {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Worktree {
    pub id: String,
    pub project_id: String,
    pub branch: String,
    pub path: PathBuf,
    #[serde(default)]
    pub is_primary: bool,
    #[serde(default)]
    pub repository_root: Option<PathBuf>,
    pub created_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectInspection {
    pub root: PathBuf,
    pub exists: bool,
    pub repository_roots: Vec<PathBuf>,
    pub repository_count: usize,
    pub can_init_git: bool,
    pub discovery_complete: bool,
    pub warning: Option<String>,
}

#[derive(Clone, Debug)]
struct Repository {
    root: PathBuf,
    common_dir: PathBuf,
}

#[derive(Clone, Debug)]
struct DiscoveredWorktree {
    path: PathBuf,
    branch: String,
    repository_root: Option<PathBuf>,
}

/// What Git says about a project's repositories and worktrees, before it is
/// compared with the store. It depends on the filesystem alone, so it can be
/// remembered while the files it was read from stay as they are.
#[derive(Clone, Debug)]
struct Discovery {
    exists: bool,
    /// Discovery finished without a warning, so the result does not lean on
    /// the roots the store already holds.
    complete: bool,
    repository_roots: Vec<PathBuf>,
    worktrees: Vec<DiscoveredWorktree>,
}

#[path = "store_scan.rs"]
pub(crate) mod scan;
use scan::Scan;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Todo,
    InProgress,
    Done,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::Done => "done",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "todo" | "open" => Ok(Self::Todo),
            "in_progress" | "doing" | "active" => Ok(Self::InProgress),
            "done" | "complete" => Ok(Self::Done),
            _ => Err(format!(
                "Unknown task status '{value}'; use todo, in_progress, or done"
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub title: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub worktree_id: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub schema_version: u32,
    pub active_project_id: Option<String>,
    pub projects: Vec<Project>,
    #[serde(default)]
    pub project_folders: Vec<ProjectFolder>,
    pub worktrees: Vec<Worktree>,
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub orca_import: Option<crate::orca_import::ImportReceipt>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema_version: 1,
            active_project_id: None,
            projects: Vec::new(),
            project_folders: Vec::new(),
            worktrees: Vec::new(),
            tasks: Vec::new(),
            orca_import: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", content = "item", rename_all = "snake_case")]
pub enum SearchHit {
    Project(Project),
    Worktree(Worktree),
    Task(Task),
}

impl State {
    pub fn project_folder(&self, selector: &str) -> Result<&ProjectFolder, String> {
        let selector = selector.trim();
        if let Some(folder) = self
            .project_folders
            .iter()
            .find(|folder| folder.id == selector)
        {
            return Ok(folder);
        }
        let name = selector.to_lowercase();
        let path = normalized_folder_path(selector);
        resolve_one(
            self.project_folders.iter().filter(|folder| {
                id_prefix(&folder.id, selector)
                    || folder.name.to_lowercase() == name
                    || (selector.contains('/')
                        && normalized_folder_path(&self.project_folder_path(&folder.id)) == path)
            }),
            "project folder",
            selector,
        )
    }

    /// A breadcrumb for display and disambiguating equal leaf names. Invalid
    /// externally edited ancestry is bounded so it cannot hang the UI.
    pub fn project_folder_path(&self, folder_id: &str) -> String {
        let mut names = Vec::new();
        let mut current = Some(folder_id);
        let mut visited = HashSet::new();
        while let Some(id) = current {
            if !visited.insert(id) {
                break;
            }
            let Some(folder) = self.project_folders.iter().find(|folder| folder.id == id) else {
                break;
            };
            names.push(folder.name.as_str());
            current = folder.parent_id.as_deref();
        }
        if names.is_empty() {
            return folder_id.to_owned();
        }
        names.reverse();
        names.join(" / ")
    }

    pub fn active_project(&self) -> Option<&Project> {
        self.active_project_id
            .as_ref()
            .and_then(|id| self.projects.iter().find(|project| &project.id == id))
    }

    pub fn project(&self, selector: &str) -> Result<&Project, String> {
        let canonical = Path::new(selector).canonicalize().ok();
        resolve_one(
            self.projects.iter().filter(|project| {
                project.id == selector
                    || id_prefix(&project.id, selector)
                    || project.name.eq_ignore_ascii_case(selector)
                    || canonical.as_ref().is_some_and(|path| path == &project.root)
            }),
            "project",
            selector,
        )
    }

    pub fn worktree(&self, selector: &str) -> Result<&Worktree, String> {
        let canonical = Path::new(selector).canonicalize().ok();
        resolve_one(
            self.worktrees
                .iter()
                .filter(|worktree| worktree_matches(worktree, selector, canonical.as_deref())),
            "worktree",
            selector,
        )
    }

    pub fn task(&self, selector: &str) -> Result<&Task, String> {
        resolve_one(
            self.tasks
                .iter()
                .filter(|task| task.id == selector || id_prefix(&task.id, selector)),
            "task",
            selector,
        )
    }

    pub fn worktrees_for(&self, project_id: &str) -> Vec<&Worktree> {
        self.worktrees
            .iter()
            .filter(|worktree| worktree.project_id == project_id)
            .collect()
    }

    /// Resolve a worktree selector inside one project first, so a branch such
    /// as `main` that exists in many projects stays unambiguous when the caller
    /// named the project. Selectors with no match in that project fall back to
    /// the global lookup, which keeps its "belongs to another project" checks.
    pub fn worktree_in_project(
        &self,
        project_id: &str,
        selector: &str,
    ) -> Result<&Worktree, String> {
        let canonical = Path::new(selector).canonicalize().ok();
        let mut scoped = self
            .worktrees
            .iter()
            .filter(|worktree| {
                worktree.project_id == project_id
                    && worktree_matches(worktree, selector, canonical.as_deref())
            })
            .peekable();
        if scoped.peek().is_none() {
            return self.worktree(selector);
        }
        resolve_one(scoped, "worktree", selector)
    }

    pub fn tasks_for_project(&self, project_id: &str) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|task| task.project_id == project_id)
            .collect()
    }

    pub fn tasks_for_worktree(&self, worktree_id: &str) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|task| task.worktree_id.as_deref() == Some(worktree_id))
            .collect()
    }

    pub fn search(&self, query: &str) -> Vec<SearchHit> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for project in &self.projects {
            if project.id.contains(&query)
                || project.name.to_lowercase().contains(&query)
                || project
                    .root
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&query)
            {
                hits.push(SearchHit::Project(project.clone()));
            }
        }
        for worktree in &self.worktrees {
            if worktree.id.contains(&query)
                || worktree.branch.to_lowercase().contains(&query)
                || worktree
                    .path
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&query)
            {
                hits.push(SearchHit::Worktree(worktree.clone()));
            }
        }
        for task in &self.tasks {
            if task.id.contains(&query)
                || task.title.to_lowercase().contains(&query)
                || task.details.to_lowercase().contains(&query)
            {
                hits.push(SearchHit::Task(task.clone()));
            }
        }
        hits
    }
}

fn resolve_one<'a, T: 'a>(
    mut matches: impl Iterator<Item = &'a T>,
    kind: &str,
    selector: &str,
) -> Result<&'a T, String> {
    let item = matches
        .next()
        .ok_or_else(|| format!("No {kind} matches '{selector}'"))?;
    if matches.next().is_some() {
        Err(format!(
            "More than one {kind} matches '{selector}'; use its UUID"
        ))
    } else {
        Ok(item)
    }
}

fn id_prefix(id: &str, selector: &str) -> bool {
    selector.len() >= 8 && id.starts_with(selector)
}

/// The one rule for what a worktree selector names, shared by the global and
/// the project-scoped lookups. `canonical` is the selector as an existing path.
fn worktree_matches(worktree: &Worktree, selector: &str, canonical: Option<&Path>) -> bool {
    worktree.id == selector
        || id_prefix(&worktree.id, selector)
        || worktree.branch == selector
        || canonical.is_some_and(|path| path == worktree.path)
}

fn project_folder_name(
    state: &State,
    name: &str,
    parent_id: Option<&str>,
    current_folder_id: Option<&str>,
) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Project folder name cannot be empty".to_owned());
    }
    if name.contains('/') {
        return Err("Project folder names cannot contain '/'".to_owned());
    }
    let folded_name = name.to_lowercase();
    if state.project_folders.iter().any(|folder| {
        Some(folder.id.as_str()) != current_folder_id
            && folder.parent_id.as_deref() == parent_id
            && folder.name.to_lowercase() == folded_name
    }) {
        return Err(format!(
            "A project folder named '{name}' already exists in this folder"
        ));
    }
    Ok(name.to_owned())
}

fn normalized_folder_path(path: &str) -> String {
    path.split('/')
        .map(|part| part.trim().to_lowercase())
        .collect::<Vec<_>>()
        .join("/")
}

fn validate_project_folder_parent(
    state: &State,
    folder_id: &str,
    parent_id: Option<&str>,
) -> Result<(), String> {
    let mut current = parent_id;
    let mut visited = HashSet::new();
    while let Some(id) = current {
        if id == folder_id {
            return Err(
                "A project folder cannot be moved into itself or one of its subfolders".to_owned(),
            );
        }
        if !visited.insert(id) {
            return Err("The destination folder has invalid circular ancestry".to_owned());
        }
        let folder = state
            .project_folders
            .iter()
            .find(|folder| folder.id == id)
            .ok_or("The destination folder has a missing parent")?;
        current = folder.parent_id.as_deref();
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A key that the file has and this build's own encoding lacks belongs to a
/// newer build, because every field this build models always serializes.
pub(crate) fn restore_missing_fields(
    original: &Map<String, Value>,
    current: &mut Map<String, Value>,
) {
    for (key, value) in original {
        if !current.contains_key(key) {
            current.insert(key.clone(), value.clone());
        }
    }
}

/// Carry unknown fields on the state, its records, and the import receipt
/// through a rewrite. Records match by ID; nested tagged values are left alone.
fn restore_unknown_state_fields(original: &Value, current: &mut Value) {
    let (Some(original), Some(current)) = (original.as_object(), current.as_object_mut()) else {
        return;
    };
    restore_missing_fields(original, current);
    for collection in ["projects", "project_folders", "worktrees", "tasks"] {
        let (Some(original), Some(current)) = (
            original.get(collection).and_then(Value::as_array),
            current.get_mut(collection).and_then(Value::as_array_mut),
        ) else {
            continue;
        };
        let originals: HashMap<&str, &Map<String, Value>> = original
            .iter()
            .filter_map(|record| {
                let record = record.as_object()?;
                Some((record.get("id")?.as_str()?, record))
            })
            .collect();
        for record in current {
            let Some(record) = record.as_object_mut() else {
                continue;
            };
            let original = record
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| originals.get(id));
            if let Some(original) = original {
                restore_missing_fields(original, record);
            }
        }
    }
    if let (Some(Value::Object(original)), Some(Value::Object(current))) =
        (original.get("orca_import"), current.get_mut("orca_import"))
    {
        restore_missing_fields(original, current);
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn open_default() -> Result<Self, String> {
        Self::open(crate::paths::riwork_home()?)
    }

    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, String> {
        let dir = dir.into();
        if dir.as_os_str().is_empty() {
            return Err("The RiWork data directory path is empty".to_owned());
        }
        crate::paths::create_private_dir(&dir)
            .map_err(|error| format!("Cannot create {}: {error}", dir.display()))?;
        Ok(Self { dir })
    }

    pub fn snapshot(&self) -> Result<State, String> {
        let lock = self.lock_file()?;
        FileExt::lock_shared(&lock).map_err(|error| format!("Cannot lock store: {error}"))?;
        self.read_state()
    }

    pub(crate) fn transaction<T>(
        &self,
        operation: impl FnOnce(&mut State) -> Result<T, String>,
    ) -> Result<T, String> {
        self.transaction_if_changed(|state| operation(state).map(|result| (result, true)))
    }

    /// Import can race with another completed import. In that case, return the
    /// saved receipt without replacing an otherwise unchanged state file.
    pub(crate) fn transaction_if_changed<T>(
        &self,
        operation: impl FnOnce(&mut State) -> Result<(T, bool), String>,
    ) -> Result<T, String> {
        let lock = self.lock_file()?;
        FileExt::lock_exclusive(&lock).map_err(|error| format!("Cannot lock store: {error}"))?;
        let (mut state, original) = self.read_state_with_original()?;
        let (result, changed) = operation(&mut state)?;
        if changed {
            self.write_state(&state, original.as_ref())?;
        }
        Ok(result)
    }

    fn lock_file(&self) -> Result<File, String> {
        let mut options = OpenOptions::new();
        // A pure lock file: never truncate it, and keep other users from
        // opening it to hold the lock.
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(self.dir.join("state.lock"))
            .map_err(|error| format!("Cannot open store lock: {error}"))
    }

    fn read_state(&self) -> Result<State, String> {
        match self.read_state_bytes()? {
            Some(data) => self.parse_state(&data),
            None => Ok(State::default()),
        }
    }

    /// Also return the file's JSON: a rewrite carries forward the fields that a
    /// newer build stored and this one does not model.
    fn read_state_with_original(&self) -> Result<(State, Option<Value>), String> {
        let Some(data) = self.read_state_bytes()? else {
            return Ok((State::default(), None));
        };
        let state = self.parse_state(&data)?;
        let original = serde_json::from_slice(&data).map_err(|error| {
            format!(
                "Cannot parse {}: {error}",
                self.dir.join("state.json").display()
            )
        })?;
        Ok((state, Some(original)))
    }

    fn read_state_bytes(&self) -> Result<Option<Vec<u8>>, String> {
        let path = self.dir.join("state.json");
        match fs::read(&path) {
            Ok(data) => Ok(Some(data)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("Cannot read {}: {error}", path.display())),
        }
    }

    fn parse_state(&self, data: &[u8]) -> Result<State, String> {
        let state: State = serde_json::from_slice(data).map_err(|error| {
            format!(
                "Cannot parse {}: {error}",
                self.dir.join("state.json").display()
            )
        })?;
        if state.schema_version != 1 {
            return Err(format!(
                "Unsupported store schema {}; this build supports schema 1",
                state.schema_version
            ));
        }
        Ok(state)
    }

    fn write_state(&self, state: &State, original: Option<&Value>) -> Result<(), String> {
        let path = self.dir.join("state.json");
        let tmp = self.dir.join(format!(".state-{}.tmp", Uuid::new_v4()));
        let mut encoded =
            serde_json::to_value(state).map_err(|error| format!("Cannot encode state: {error}"))?;
        if let Some(original) = original {
            restore_unknown_state_fields(original, &mut encoded);
        }
        let write = || -> Result<(), String> {
            // The state names projects and paths, so keep it owner-only.
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&tmp)
                .map_err(|error| format!("Cannot create {}: {error}", tmp.display()))?;
            serde_json::to_writer_pretty(&mut file, &encoded)
                .map_err(|error| format!("Cannot encode state: {error}"))?;
            file.write_all(b"\n")
                .map_err(|error| format!("Cannot write state: {error}"))?;
            file.sync_all()
                .map_err(|error| format!("Cannot sync state: {error}"))?;
            fs::rename(&tmp, &path)
                .map_err(|error| format!("Cannot replace {}: {error}", path.display()))?;
            File::open(&self.dir)
                .and_then(|dir| dir.sync_all())
                .map_err(|error| format!("Cannot sync {}: {error}", self.dir.display()))
        };
        let result = write();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// Inspect without changing the filesystem or project registry. Repository
    /// roots are deduplicated across linked worktrees of the same Git repo.
    pub fn inspect_project(root: impl AsRef<Path>) -> Result<ProjectInspection, String> {
        Self::inspect_scan(root.as_ref(), &mut Scan::fresh())
    }

    fn inspect_scan(root: &Path, scan: &mut Scan) -> Result<ProjectInspection, String> {
        let (root, ancestor) = resolve_project_path(root)?;
        let exists = root.is_dir();
        let mut repositories = BTreeMap::new();
        let mut warnings = Vec::new();
        match scan.probe_ancestor(&ancestor) {
            Ok(Some(repository)) => {
                repositories.insert(repository.common_dir.clone(), repository);
            }
            Ok(None) => {}
            Err(error) => warnings.push(error),
        }
        if exists {
            let mut pending = vec![(root.clone(), 0usize)];
            let mut visited = 0usize;
            let mut scheduled = 1usize;
            'discovery: while let Some((directory, depth)) = pending.pop() {
                visited += 1;
                if visited > 20_000 {
                    warnings
                        .push("Repository discovery reached the 20,000-directory limit".to_owned());
                    break;
                }
                scan.walk(&directory);
                if directory != ancestor && has_git_marker(&directory) {
                    match scan.probe(&directory) {
                        Ok(Some(repository)) => {
                            repositories.insert(repository.common_dir.clone(), repository);
                        }
                        Ok(None) => warnings.push(format!(
                            "Git metadata in {} is not a usable repository",
                            directory.display()
                        )),
                        Err(error) => warnings.push(error),
                    }
                }
                let entries = match fs::read_dir(&directory) {
                    Ok(entries) => entries,
                    Err(error) => {
                        warnings.push(format!("Cannot inspect {}: {error}", directory.display()));
                        continue;
                    }
                };
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => {
                            warnings
                                .push(format!("Cannot inspect {}: {error}", directory.display()));
                            continue;
                        }
                    };
                    let kind = match entry.file_type() {
                        Ok(kind) => kind,
                        Err(error) => {
                            warnings.push(format!(
                                "Cannot inspect {}: {error}",
                                entry.path().display()
                            ));
                            continue;
                        }
                    };
                    if !kind.is_dir()
                        || kind.is_symlink()
                        || excluded_repository_directory(&entry.file_name())
                    {
                        continue;
                    }
                    if depth >= 24 {
                        warnings.push(format!(
                            "Repository discovery reached its depth limit at {}",
                            entry.path().display()
                        ));
                        continue;
                    }
                    if scheduled >= 20_000 {
                        warnings.push(
                            "Repository discovery reached the 20,000-directory limit".to_owned(),
                        );
                        break 'discovery;
                    }
                    scheduled += 1;
                    pending.push((entry.path(), depth + 1));
                }
            }
        }
        let mut repository_roots: Vec<_> = repositories
            .into_values()
            .map(|repository| repository.root)
            .collect();
        repository_roots.sort();
        let repository_count = repository_roots.len();
        let discovery_complete = warnings.is_empty();
        let warning = (!warnings.is_empty())
            .then(|| warnings.into_iter().take(3).collect::<Vec<_>>().join("; "));
        Ok(ProjectInspection {
            root,
            exists,
            repository_roots,
            repository_count,
            can_init_git: repository_count == 0 && discovery_complete,
            discovery_complete,
            warning,
        })
    }

    /// Create a directory if needed, initialize Git only for a plain project,
    /// and register it. Existing repos are never wrapped in another repository.
    pub fn create_project(
        &self,
        root: impl AsRef<Path>,
        name: Option<&str>,
        init_git: bool,
    ) -> Result<Project, String> {
        let before = Self::inspect_project(root.as_ref())?;
        fs::create_dir_all(&before.root)
            .map_err(|error| format!("Cannot create {}: {error}", before.root.display()))?;
        let mut inspection = Self::inspect_project(&before.root)?;
        if init_git && inspection.repository_count == 0 {
            if !inspection.can_init_git {
                return Err(format!(
                    "Cannot safely initialize Git: {}. Use --no-git to register a plain folder.",
                    inspection
                        .warning
                        .as_deref()
                        .unwrap_or("repository discovery is incomplete")
                ));
            }
            let output = git_command(&inspection.root)
                .arg("init")
                .output()
                .map_err(|error| format!("Cannot launch git: {error}"))?;
            if !output.status.success() {
                return Err(format!(
                    "git init failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            inspection = Self::inspect_project(&inspection.root)?;
        }
        self.register_project(inspection, name)
    }

    /// Passive registration; unlike create_project, this never initializes Git.
    pub fn add_project(
        &self,
        root: impl AsRef<Path>,
        name: Option<&str>,
    ) -> Result<Project, String> {
        let inspection = Self::inspect_project(root)?;
        if !inspection.exists {
            return Err(format!(
                "Project directory does not exist: {}",
                inspection.root.display()
            ));
        }
        self.register_project(inspection, name)
    }

    fn register_project(
        &self,
        mut inspection: ProjectInspection,
        name: Option<&str>,
    ) -> Result<Project, String> {
        if !inspection.discovery_complete {
            if let Some(previous) = self
                .snapshot()?
                .projects
                .iter()
                .find(|project| project.root == inspection.root)
            {
                inspection.repository_roots.extend(
                    previous
                        .repository_roots
                        .iter()
                        .filter(|path| path.is_dir())
                        .cloned(),
                );
                inspection.repository_roots.sort();
                inspection.repository_roots.dedup();
            }
        }
        let root = inspection.root;
        let explicit_name = name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        let name = explicit_name
            .clone()
            .or_else(|| {
                root.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "project".to_owned());
        let discovered =
            project_worktrees(&root, &inspection.repository_roots, &mut Scan::fresh())?;
        let project = self.transaction(|state| {
            let project = if let Some(project) = state
                .projects
                .iter_mut()
                .find(|project| project.root == root)
            {
                if let Some(name) = explicit_name {
                    project.name = name;
                }
                project.repository_roots = inspection.repository_roots.clone();
                project.clone()
            } else {
                let project = Project {
                    id: Uuid::new_v4().to_string(),
                    name,
                    root: root.clone(),
                    repository_roots: inspection.repository_roots.clone(),
                    folder_id: None,
                    notify_on_agent_done: false,
                    codex_account: ProjectCodexAccount::default(),
                    created_at: now(),
                };
                if state.active_project_id.is_none() {
                    state.active_project_id = Some(project.id.clone());
                }
                state.projects.push(project.clone());
                project
            };
            merge_worktrees(state, &project, discovered);
            Ok(project)
        })?;
        Ok(project)
    }

    pub fn use_project(&self, selector: &str) -> Result<Project, String> {
        self.transaction(|state| {
            let project = state.project(selector)?.clone();
            state.active_project_id = Some(project.id.clone());
            Ok(project)
        })
    }

    pub fn create_project_folder(&self, name: &str) -> Result<ProjectFolder, String> {
        self.create_project_folder_in(name, None)
    }

    pub fn create_project_folder_in(
        &self,
        name: &str,
        parent: Option<&str>,
    ) -> Result<ProjectFolder, String> {
        self.transaction(|state| {
            let parent_id = parent
                .map(|selector| {
                    state
                        .project_folder(selector)
                        .map(|folder| folder.id.clone())
                })
                .transpose()?;
            let id = Uuid::new_v4().to_string();
            validate_project_folder_parent(state, &id, parent_id.as_deref())?;
            let name = project_folder_name(state, name, parent_id.as_deref(), None)?;
            let folder = ProjectFolder {
                id,
                name,
                parent_id,
                created_at: now(),
            };
            state.project_folders.push(folder.clone());
            Ok(folder)
        })
    }

    pub fn rename_project_folder(
        &self,
        selector: &str,
        name: &str,
    ) -> Result<ProjectFolder, String> {
        self.transaction(|state| {
            let folder = state.project_folder(selector)?.clone();
            let folder_id = folder.id;
            let name =
                project_folder_name(state, name, folder.parent_id.as_deref(), Some(&folder_id))?;
            let folder = state
                .project_folders
                .iter_mut()
                .find(|folder| folder.id == folder_id)
                .expect("resolved folder remains in the transaction");
            folder.name = name;
            Ok(folder.clone())
        })
    }

    pub fn move_project_folder(
        &self,
        selector: &str,
        parent: Option<&str>,
    ) -> Result<ProjectFolder, String> {
        self.transaction(|state| {
            let folder = state.project_folder(selector)?.clone();
            let parent_id = parent
                .map(|selector| {
                    state
                        .project_folder(selector)
                        .map(|parent| parent.id.clone())
                })
                .transpose()?;
            validate_project_folder_parent(state, &folder.id, parent_id.as_deref())?;
            project_folder_name(state, &folder.name, parent_id.as_deref(), Some(&folder.id))?;
            let folder = state
                .project_folders
                .iter_mut()
                .find(|item| item.id == folder.id)
                .expect("resolved folder remains in the transaction");
            folder.parent_id = parent_id;
            Ok(folder.clone())
        })
    }

    /// Promote direct projects and subfolders to the removed folder's parent;
    /// nothing on disk is removed or relocated, and descendants retain their IDs.
    pub fn remove_project_folder(&self, selector: &str) -> Result<(), String> {
        self.transaction(|state| {
            let removed = state.project_folder(selector)?.clone();
            let mut promoted_names = HashSet::new();
            for child in state.project_folders.iter().filter(|folder| folder.parent_id.as_deref() == Some(&removed.id)) {
                let name = child.name.to_lowercase();
                if !promoted_names.insert(name.clone()) || state.project_folders.iter().any(|sibling| {
                    sibling.id != removed.id
                        && sibling.id != child.id
                        && sibling.parent_id == removed.parent_id
                        && sibling.name.to_lowercase() == name
                }) {
                    return Err(format!("Cannot remove folder '{}': promoting subfolder '{}' would duplicate a folder in its parent. Move or rename the conflicting folder first.", removed.name, child.name));
                }
            }
            state
                .project_folders
                .retain(|folder| folder.id != removed.id);
            for folder in &mut state.project_folders {
                if folder.parent_id.as_deref() == Some(&removed.id) {
                    folder.parent_id = removed.parent_id.clone();
                }
            }
            for project in &mut state.projects {
                if project.folder_id.as_deref() == Some(&removed.id) {
                    project.folder_id = removed.parent_id.clone();
                }
            }
            Ok(())
        })
    }

    /// Reassign only organization metadata inside the transaction, preserving
    /// the latest project name even if another window just renamed it.
    pub fn move_project_to_folder(
        &self,
        project_selector: &str,
        folder: Option<&str>,
    ) -> Result<Project, String> {
        self.transaction(|state| {
            let project_id = state.project(project_selector)?.id.clone();
            let folder_id = folder
                .map(|selector| {
                    state
                        .project_folder(selector)
                        .map(|folder| folder.id.clone())
                })
                .transpose()?;
            let project = state
                .projects
                .iter_mut()
                .find(|project| project.id == project_id)
                .expect("resolved project remains in the transaction");
            project.folder_id = folder_id;
            Ok(project.clone())
        })
    }

    /// Rename inside the transaction without touching the folder, so a folder
    /// move made meanwhile by another window or command is not reverted.
    pub fn rename_project(&self, project_selector: &str, name: &str) -> Result<Project, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Project name cannot be empty".to_owned());
        }
        self.transaction(|state| {
            let project_id = state.project(project_selector)?.id.clone();
            let project = state
                .projects
                .iter_mut()
                .find(|project| project.id == project_id)
                .expect("resolved project remains in the transaction");
            project.name = name.to_owned();
            Ok(project.clone())
        })
    }

    /// Update display metadata only; the project's directory and worktrees stay put.
    pub fn update_project_metadata(
        &self,
        project_selector: &str,
        name: &str,
        folder_id: Option<&str>,
    ) -> Result<Project, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Project name cannot be empty".to_owned());
        }
        self.transaction(|state| {
            let project_id = state.project(project_selector)?.id.clone();
            let folder_id = folder_id
                .map(|selector| {
                    state
                        .project_folder(selector)
                        .map(|folder| folder.id.clone())
                })
                .transpose()?;
            let project = state
                .projects
                .iter_mut()
                .find(|project| project.id == project_id)
                .expect("resolved project remains in the transaction");
            project.name = name.to_owned();
            project.folder_id = folder_id;
            Ok(project.clone())
        })
    }

    /// Change only this project's completion notification preference, keeping
    /// the latest display metadata and workspace records from every window.
    pub fn set_project_notifications(
        &self,
        selector: &str,
        enabled: bool,
    ) -> Result<Project, String> {
        self.transaction_if_changed(|state| {
            let project_id = state.project(selector)?.id.clone();
            let project = state
                .projects
                .iter_mut()
                .find(|project| project.id == project_id)
                .expect("resolved project remains in the transaction");
            let changed = project.notify_on_agent_done != enabled;
            project.notify_on_agent_done = enabled;
            Ok((project.clone(), changed))
        })
    }

    /// Change just the account preference, preserving concurrent edits to the
    /// project's other metadata. Saved IDs must resolve through Orca's public
    /// metadata cache before they can be selected.
    pub fn set_project_codex_account(
        &self,
        selector: &str,
        choice: ProjectCodexAccount,
    ) -> Result<Project, String> {
        if let ProjectCodexAccount::Saved(id) = &choice {
            crate::codex_accounts::resolve_launch_binding(&self.dir, Some(id))?;
        }
        self.transaction_if_changed(|state| {
            let project_id = state.project(selector)?.id.clone();
            let project = state
                .projects
                .iter_mut()
                .find(|project| project.id == project_id)
                .expect("resolved project remains in the transaction");
            let changed = project.codex_account != choice;
            project.codex_account = choice;
            Ok((project.clone(), changed))
        })
    }

    /// Discover worktrees created outside RiWork while preserving UUIDs used by
    /// tasks and shell sessions. Plain directories retain their root worktree.
    pub fn sync_worktrees(&self, project_selector: &str) -> Result<Vec<Worktree>, String> {
        self.sync_worktrees_with(project_selector, false)
    }

    /// The periodic refresh a window runs for its project. Unlike
    /// `sync_worktrees` it may return without looking at anything: the GUI is
    /// one process, so a project another window synced moments ago, or is
    /// syncing now, is left to that sync, and a project whose files are
    /// unchanged since the last scan is not scanned again. Worktrees created
    /// outside the app still show up within a refresh period or two.
    pub fn refresh_worktrees(&self, project_selector: &str) -> Result<(), String> {
        let key = format!("{}\n{project_selector}", self.dir.display());
        scan::SYNCS
            .run(&key, scan::SYNC_MIN_GAP, || {
                self.sync_worktrees_with(project_selector, true).map(drop)
            })
            .unwrap_or(Ok(()))
    }

    /// With `cached`, Git is not asked what an earlier scan already learned
    /// from files that have not changed since.
    fn sync_worktrees_with(
        &self,
        project_selector: &str,
        cached: bool,
    ) -> Result<Vec<Worktree>, String> {
        let state = self.snapshot()?;
        let project = state.project(project_selector)?.clone();
        let Discovery {
            exists,
            repository_roots,
            worktrees: discovered,
            ..
        } = discover_project(&project, cached)?;
        if !exists {
            return Ok(state
                .worktrees_for(&project.id)
                .into_iter()
                .cloned()
                .collect());
        }
        let unchanged = project.repository_roots == repository_roots
            && discovered.iter().all(|found| {
                state.worktrees.iter().any(|worktree| {
                    worktree.project_id == project.id
                        && worktree.path == found.path
                        && worktree.branch == found.branch
                        && worktree.repository_root == found.repository_root
                        && worktree.is_primary == (found.path == project.root)
                })
            });
        if unchanged {
            return Ok(state
                .worktrees_for(&project.id)
                .into_iter()
                .cloned()
                .collect());
        }
        self.transaction(|state| {
            state.project(&project.id)?;
            state
                .projects
                .iter_mut()
                .find(|item| item.id == project.id)
                .unwrap()
                .repository_roots = repository_roots;
            merge_worktrees(state, &project, discovered);
            Ok(state
                .worktrees_for(&project.id)
                .into_iter()
                .cloned()
                .collect())
        })
    }

    /// Forget a removed worktree once no task still points at it. This never
    /// deletes files or runs `git worktree remove`.
    pub fn forget_missing_worktree(&self, selector: &str) -> Result<Worktree, String> {
        self.transaction(|state| {
            let worktree = state.worktree(selector)?.clone();
            if worktree.is_primary || worktree.path.exists() {
                return Err(format!(
                    "Worktree {} still exists or is the project root",
                    worktree.id
                ));
            }
            if state
                .tasks
                .iter()
                .any(|task| task.worktree_id.as_deref() == Some(worktree.id.as_str()))
            {
                return Err(format!(
                    "Tasks are still assigned to worktree {}",
                    worktree.id
                ));
            }
            state.worktrees.retain(|item| item.id != worktree.id);
            Ok(worktree)
        })
    }

    pub fn add_task(
        &self,
        project_selector: &str,
        title: &str,
        details: &str,
    ) -> Result<Task, String> {
        let title = title.trim();
        if title.is_empty() {
            return Err("Task title cannot be empty".to_owned());
        }
        self.transaction(|state| {
            let project_id = state.project(project_selector)?.id.clone();
            let timestamp = now();
            let task = Task {
                id: Uuid::new_v4().to_string(),
                project_id,
                title: title.to_owned(),
                details: details.to_owned(),
                status: TaskStatus::Todo,
                worktree_id: None,
                created_at: timestamp,
                updated_at: timestamp,
            };
            state.tasks.push(task.clone());
            Ok(task)
        })
    }

    pub fn assign_tasks(
        &self,
        worktree_selector: &str,
        task_selectors: &[String],
    ) -> Result<Vec<Task>, String> {
        if task_selectors.is_empty() {
            return Err("Give at least one task UUID to assign".to_owned());
        }
        self.transaction(|state| {
            let worktree = state.worktree(worktree_selector)?.clone();
            let mut ids = HashSet::new();
            for selector in task_selectors {
                let task = state.task(selector)?;
                if task.project_id != worktree.project_id {
                    return Err(format!("Task {} belongs to another project", task.id));
                }
                ids.insert(task.id.clone());
            }
            let timestamp = now();
            let mut changed = Vec::new();
            for task in &mut state.tasks {
                if ids.contains(&task.id) {
                    task.worktree_id = Some(worktree.id.clone());
                    task.updated_at = timestamp;
                    changed.push(task.clone());
                }
            }
            Ok(changed)
        })
    }

    pub fn unassign_tasks(&self, task_selectors: &[String]) -> Result<Vec<Task>, String> {
        if task_selectors.is_empty() {
            return Err("Give at least one task UUID to unassign".to_owned());
        }
        self.transaction(|state| {
            let mut ids = HashSet::new();
            for selector in task_selectors {
                ids.insert(state.task(selector)?.id.clone());
            }
            let timestamp = now();
            let mut changed = Vec::new();
            for task in &mut state.tasks {
                if ids.contains(&task.id) {
                    task.worktree_id = None;
                    task.updated_at = timestamp;
                    changed.push(task.clone());
                }
            }
            Ok(changed)
        })
    }

    pub fn set_task_status(&self, selector: &str, status: TaskStatus) -> Result<Task, String> {
        self.transaction(|state| {
            let id = state.task(selector)?.id.clone();
            let task = state.tasks.iter_mut().find(|task| task.id == id).unwrap();
            task.status = status;
            task.updated_at = now();
            Ok(task.clone())
        })
    }

    pub fn create_worktree(
        &self,
        project_selector: &str,
        branch: &str,
        requested_path: Option<&Path>,
        base: Option<&str>,
    ) -> Result<Worktree, String> {
        self.create_worktree_in_repo(project_selector, branch, requested_path, base, None)
    }

    pub fn create_worktree_in_repo(
        &self,
        project_selector: &str,
        branch: &str,
        requested_path: Option<&Path>,
        base: Option<&str>,
        repository_selector: Option<&str>,
    ) -> Result<Worktree, String> {
        if branch.trim().is_empty() || branch.starts_with('-') {
            return Err("Give a nonempty Git branch name that does not start with '-'".to_owned());
        }
        self.sync_worktrees(project_selector)?;
        // Git may run checkout hooks and LFS for a long time, and a hook can call
        // `riwork` itself, so the store lock covers only resolving the inputs
        // here and recording the result below.
        let state = self.snapshot()?;
        let project = state.project(project_selector)?.clone();
        let repository_root = select_repository(&state, &project, repository_selector)?;
        let validation = git_command(&repository_root)
            .args(["check-ref-format", "--branch"])
            .arg(branch)
            .output()
            .map_err(|error| format!("Cannot launch git: {error}"))?;
        if !validation.status.success() {
            return Err(format!("Invalid Git branch name: {branch}"));
        }
        let existing = git_command(&repository_root)
            .args(["show-ref", "--verify", "--quiet"])
            .arg(format!("refs/heads/{branch}"))
            .status()
            .map_err(|error| format!("Cannot launch git: {error}"))?
            .success();
        let commit_ref = if existing {
            format!("refs/heads/{branch}")
        } else {
            base.unwrap_or("HEAD").to_owned()
        };
        let commit = git_command(&repository_root)
            .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
            .arg(format!("{commit_ref}^{{commit}}"))
            .output()
            .map_err(|error| format!("Cannot launch git: {error}"))?;
        if !commit.status.success() {
            return Err(if !existing && base.is_none() {
                "Repository has no initial commit; commit first or choose an existing --base before creating a worktree".to_owned()
            } else {
                format!("Worktree base '{commit_ref}' does not resolve to a commit")
            });
        }
        let repo_suffix = if project.repository_roots.len() > 1 {
            format!(
                "-{}",
                slug(
                    &repository_root
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                )
            )
        } else {
            String::new()
        };
        let default_path = project.root.parent().unwrap_or(&project.root).join(format!(
            "{}{repo_suffix}-{}",
            slug(&project.name),
            slug(branch)
        ));
        let path = requested_path.unwrap_or(&default_path);
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            env::current_dir()
                .map_err(|error| format!("Cannot read current directory: {error}"))?
                .join(path)
        };
        if fs::symlink_metadata(&path).is_ok() {
            return Err(format!("Worktree path already exists: {}", path.display()));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Cannot create worktree parent {}: {error}",
                    parent.display()
                )
            })?;
        }
        let mut command = git_command(&repository_root);
        command.args(["worktree", "add"]);
        if existing {
            command.arg(&path).arg(branch);
        } else {
            command
                .arg("-b")
                .arg(branch)
                .arg(&path)
                .arg(String::from_utf8_lossy(&commit.stdout).trim());
        }
        let output = command
            .output()
            .map_err(|error| format!("Cannot launch git: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let path = path.canonicalize().unwrap_or(path);
        // Another window's sync may already have discovered the new worktree.
        self.transaction_if_changed(|state| {
            state.project(&project.id).map_err(|error| {
                format!(
                    "{error}; the worktree was created at {} and will be registered by a sync",
                    path.display()
                )
            })?;
            if let Some(recorded) = state
                .worktrees
                .iter()
                .find(|worktree| worktree.project_id == project.id && worktree.path == path)
            {
                return Ok((recorded.clone(), false));
            }
            let worktree = Worktree {
                id: Uuid::new_v4().to_string(),
                project_id: project.id.clone(),
                branch: branch.to_owned(),
                path: path.clone(),
                is_primary: false,
                repository_root: Some(repository_root.clone()),
                created_at: now(),
            };
            state.worktrees.push(worktree.clone());
            Ok((worktree, true))
        })
    }
}

fn slug(value: &str) -> String {
    let slug = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "worktree".to_owned()
    } else {
        slug.to_owned()
    }
}

#[cfg(test)]
thread_local! {
    /// Git commands built on this thread, so a test can prove a refresh ran none.
    static GIT_COMMANDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn git_command(root: &Path) -> Command {
    #[cfg(test)]
    GIT_COMMANDS.with(|count| count.set(count.get() + 1));
    let mut command = Command::new("git");
    command.arg("-C").arg(root);
    command.env("LC_ALL", "C");
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CEILING_DIRECTORIES",
    ] {
        command.env_remove(variable);
    }
    command
}

fn resolve_project_path(path: &Path) -> Result<(PathBuf, PathBuf), String> {
    if path.as_os_str().is_empty() {
        return Err("Project path cannot be empty".to_owned());
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        env::current_dir()
            .map_err(|error| format!("Cannot read current directory: {error}"))?
            .join(path)
    };
    let mut ancestor = absolute;
    let mut missing = Vec::new();
    while !ancestor.exists() {
        match fs::symlink_metadata(&ancestor) {
            Ok(_) => {
                return Err(format!(
                    "Project path contains a dangling symlink: {}",
                    ancestor.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Cannot inspect project path {}: {error}",
                    ancestor.display()
                ));
            }
        }
        let name = ancestor
            .file_name()
            .ok_or_else(|| format!("Cannot resolve project path {}", path.display()))?
            .to_owned();
        missing.push(name);
        if !ancestor.pop() {
            return Err(format!("Cannot resolve project path {}", path.display()));
        }
    }
    let ancestor = ancestor
        .canonicalize()
        .map_err(|error| format!("Cannot resolve {}: {error}", ancestor.display()))?;
    if !ancestor.is_dir() {
        return Err(format!(
            "Project path is not a directory: {}",
            ancestor.display()
        ));
    }
    let mut root = ancestor.clone();
    for component in missing.into_iter().rev() {
        root.push(component);
    }
    Ok((root, ancestor))
}

fn excluded_repository_directory(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            ".git"
                | "node_modules"
                | "target"
                | ".venv"
                | "venv"
                | ".cache"
                | ".Trash"
                | "dist"
                | "build"
        )
    )
}

fn has_git_marker(root: &Path) -> bool {
    fs::symlink_metadata(root.join(".git")).is_ok()
        || (root.join("HEAD").is_file()
            && root.join("objects").is_dir()
            && root.join("refs").is_dir())
}

fn output_path(bytes: &[u8]) -> PathBuf {
    let text = String::from_utf8_lossy(bytes);
    PathBuf::from(text.strip_suffix('\n').unwrap_or(&text))
}

fn probe_repository(root: &Path) -> Result<Option<Repository>, String> {
    let output = git_command(root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| format!("Cannot launch Git for repository discovery: {error}"))?;
    let working_root = if output.status.success() {
        output_path(&output.stdout)
    } else if has_git_marker(root) {
        let bare = git_command(root)
            .args(["rev-parse", "--is-bare-repository"])
            .output()
            .map_err(|error| format!("Cannot launch git: {error}"))?;
        if !bare.status.success() || String::from_utf8_lossy(&bare.stdout).trim() != "true" {
            return Err(format!(
                "Cannot inspect Git metadata in {}: {}",
                root.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        root.to_owned()
    } else if String::from_utf8_lossy(&output.stderr).contains("not a git repository") {
        return Ok(None);
    } else {
        return Err(format!(
            "Cannot inspect {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    };
    let common = git_command(&working_root)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .map_err(|error| format!("Cannot launch git: {error}"))?;
    if !common.status.success() {
        return Err(format!(
            "Cannot inspect repository {}: {}",
            working_root.display(),
            String::from_utf8_lossy(&common.stderr).trim()
        ));
    }
    let common_dir = output_path(&common.stdout);
    let common_dir = common_dir.canonicalize().unwrap_or(common_dir);
    let worktrees = discover_git_worktrees(&working_root)?;
    let root = worktrees
        .first()
        .map(|worktree| worktree.path.clone())
        .unwrap_or(working_root);
    Ok(Some(Repository { root, common_dir }))
}

fn discover_git_worktrees(root: &Path) -> Result<Vec<DiscoveredWorktree>, String> {
    let output = git_command(root)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
        .map_err(|error| format!("Cannot launch git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Cannot list worktrees for {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut found = Vec::new();
    let mut path = None;
    let mut branch = None;
    for attribute in output.stdout.split(|byte| *byte == 0) {
        let attribute = String::from_utf8_lossy(attribute);
        if let Some(value) = attribute.strip_prefix("worktree ") {
            if let Some(path) = path.take() {
                found.push(DiscoveredWorktree {
                    path,
                    branch: branch.take().unwrap_or_else(|| "unknown".to_owned()),
                    repository_root: None,
                });
            }
            let value = PathBuf::from(value);
            path = Some(value.canonicalize().unwrap_or(value));
        } else if let Some(value) = attribute.strip_prefix("branch refs/heads/") {
            branch = Some(value.to_owned());
        } else if attribute == "detached" || attribute == "bare" {
            branch = Some(attribute.into_owned());
        }
    }
    if let Some(path) = path {
        found.push(DiscoveredWorktree {
            path,
            branch: branch.unwrap_or_else(|| "unknown".to_owned()),
            repository_root: None,
        });
    }
    Ok(found)
}

fn discover_project(project: &Project, cached: bool) -> Result<Discovery, String> {
    if !cached {
        return scan_project(project, &mut Scan::fresh());
    }
    if let Some(found) = scan::cached_discovery(&project.root) {
        return Ok(found);
    }
    let mut scan = Scan::cached();
    scan.root(&project.root);
    let found = scan_project(project, &mut scan)?;
    scan::remember_discovery(&project.root, scan, &found);
    Ok(found)
}

fn scan_project(project: &Project, scan: &mut Scan) -> Result<Discovery, String> {
    let inspection = Store::inspect_scan(&project.root, scan)?;
    let complete = inspection.discovery_complete;
    if !inspection.exists {
        return Ok(Discovery {
            exists: false,
            complete,
            repository_roots: Vec::new(),
            worktrees: Vec::new(),
        });
    }
    let mut repository_roots = inspection.repository_roots;
    if !complete {
        repository_roots.extend(
            project
                .repository_roots
                .iter()
                .filter(|path| path.is_dir())
                .cloned(),
        );
        repository_roots.sort();
        repository_roots.dedup();
    }
    let worktrees = project_worktrees(&project.root, &repository_roots, scan)?;
    Ok(Discovery {
        exists: true,
        complete,
        repository_roots,
        worktrees,
    })
}

fn project_worktrees(
    root: &Path,
    repository_roots: &[PathBuf],
    scan: &mut Scan,
) -> Result<Vec<DiscoveredWorktree>, String> {
    let mut found = BTreeMap::new();
    let root_repository = repository_roots
        .iter()
        .filter(|repository| root.starts_with(repository))
        .max_by_key(|repository| repository.as_os_str().len())
        .cloned();
    let root_branch = root_repository
        .as_ref()
        .and_then(|_| {
            git_command(root)
                .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        })
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "root".to_owned());
    found.insert(
        root.to_owned(),
        DiscoveredWorktree {
            path: root.to_owned(),
            branch: root_branch,
            repository_root: root_repository,
        },
    );
    for repository in repository_roots {
        for mut worktree in scan.worktrees(repository)? {
            worktree.repository_root = Some(repository.clone());
            found.insert(worktree.path.clone(), worktree);
        }
    }
    Ok(found.into_values().collect())
}

fn merge_worktrees(state: &mut State, project: &Project, discovered: Vec<DiscoveredWorktree>) {
    for found in discovered {
        let is_primary = found.path == project.root;
        if let Some(worktree) = state
            .worktrees
            .iter_mut()
            .find(|worktree| worktree.project_id == project.id && worktree.path == found.path)
        {
            worktree.branch = found.branch;
            worktree.is_primary = is_primary;
            worktree.repository_root = found.repository_root;
        } else {
            state.worktrees.push(Worktree {
                id: Uuid::new_v4().to_string(),
                project_id: project.id.clone(),
                branch: found.branch,
                path: found.path,
                is_primary,
                repository_root: found.repository_root,
                created_at: now(),
            });
        }
    }
}

fn select_repository(
    state: &State,
    project: &Project,
    selector: Option<&str>,
) -> Result<PathBuf, String> {
    if let Some(selector) = selector {
        let absolute = Path::new(selector).canonicalize().ok();
        let relative = project.root.join(selector).canonicalize().ok();
        let mut matches = project.repository_roots.iter().filter(|repository| {
            absolute.as_ref() == Some(*repository)
                || relative.as_ref() == Some(*repository)
                || repository.file_name().is_some_and(|name| name == selector)
        });
        if let Some(repository) = matches.next() {
            if matches.next().is_some() {
                return Err(format!(
                    "Repository '{selector}' is ambiguous; use its full path"
                ));
            }
            return Ok(repository.clone());
        }
        if let Ok(worktree) = state.worktree(selector) {
            let direct_selector = worktree.id == selector
                || id_prefix(&worktree.id, selector)
                || absolute.as_ref() == Some(&worktree.path)
                || relative.as_ref() == Some(&worktree.path);
            if worktree.project_id == project.id && direct_selector {
                if let Some(repository) = &worktree.repository_root {
                    if project.repository_roots.contains(repository) {
                        return Ok(repository.clone());
                    }
                }
            }
        }
        return Err(format!(
            "No repository in this project matches '{selector}'"
        ));
    }
    match project.repository_roots.as_slice() {
        [repository] => Ok(repository.clone()),
        [] => Err(format!(
            "Project {} is a plain folder; initialize Git before creating a Git worktree",
            project.name
        )),
        repositories => Err(format!(
            "Project {} contains {} repositories; choose --repo PATH: {}",
            project.name,
            repositories.len(),
            repositories
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
