//! Add-ins: TSX extensions of RiWork. An add-in is a folder with an `addin.json` manifest and
//! NSH TSX sources (`App.tsx`), and optionally native workers: binaries or scripts it ships,
//! run as processes of their own.
//!
//! - Project add-ins live in the project, `<root>/.ontack/addins/<folder>`, and travel with
//!   its repository.
//! - Personal add-ins live in `RIWORK_HOME/addins/<folder>`, for every project.
//!
//! An add-in runs in an engine of its own and reaches RiWork only through the add-in API, for
//! what its manifest's `permissions` grant. Nothing of an add-in runs until it is trusted. The
//! grant covers its permissions, network origins, workers, tools and the files its workers
//! run (`grant_digest`), and is asked for again when any of those change. Its TSX may change
//! freely (an agent writing it, live): what it can do is still only what was granted.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// Where a project keeps its add-ins, from its root.
pub const PROJECT_DIR: &str = ".ontack/addins";
pub const MANIFEST: &str = "addin.json";
/// The add-in API this build serves; a manifest asking for a later one is refused.
pub const API_VERSION: u32 = 1;
/// The most an add-in may keep in its storage.
pub const STORAGE_LIMIT: usize = 1 << 20;

/// What an add-in may do through the add-in API. Reading its own storage and settings, and
/// opening its own views, needs nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Permission {
    /// The project, its worktrees and tasks, and opening its files.
    #[serde(rename = "project.read")]
    ProjectRead,
    /// Adding tasks and changing their status.
    #[serde(rename = "tasks.write")]
    TasksWrite,
    /// Running a command in a shell tab of its own, which the person sees.
    #[serde(rename = "shell.run")]
    ShellRun,
    /// Notifications.
    #[serde(rename = "notify")]
    Notify,
    /// A status bar item.
    #[serde(rename = "status")]
    Status,
    /// Requests to the origins in `network`.
    #[serde(rename = "network")]
    Network,
}

impl Permission {
    pub fn describe(self) -> &'static str {
        match self {
            Self::ProjectRead => "Read the project, its worktrees and tasks, and open its files",
            Self::TasksWrite => "Add tasks and change their status",
            Self::ShellRun => "Run commands in shell tabs you can see",
            Self::Notify => "Show notifications",
            Self::Status => "Show an item in the status bar",
            Self::Network => "Connect to the servers it names",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingKind {
    String,
    Number,
    Boolean,
}

/// A value the person sets for an add-in (a scanner's URL): this is what makes a company's
/// add-in useful to another company.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingSpec {
    #[serde(rename = "type")]
    pub kind: SettingKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

/// A process the add-in ships: `command[0]` is a file in the add-in's folder, run with the
/// rest as arguments in the folder, speaking JSON lines on stdin and stdout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerSpec {
    pub id: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// A tool agents can call (through `riwork mcp`): a worker's method.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub worker: String,
    pub method: String,
    #[serde(default = "empty_schema")]
    pub input_schema: Value,
}

fn empty_schema() -> Value {
    json!({"type": "object", "properties": {}})
}

/// An action for the command list; choosing it sends the add-in a `command` event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub id: String,
    pub title: String,
}

fn api_v1() -> u32 {
    1
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default = "api_v1")]
    pub api: u32,
    #[serde(default)]
    pub description: String,
    pub views: Vec<View>,
    #[serde(default)]
    pub permissions: BTreeSet<Permission>,
    /// The origins (`https://scanner.example.com`) it may reach, with `network`.
    #[serde(default)]
    pub network: Vec<String>,
    #[serde(default)]
    pub settings: BTreeMap<String, SettingSpec>,
    #[serde(default)]
    pub workers: Vec<WorkerSpec>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    /// Contracts it offers other add-ins (`viewer.field@1`), and those it needs.
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
}

/// `a-z0-9`, separated by single dots or dashes: `acme.security-dashboard`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.split(['.', '-']).all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A path inside the add-in's folder: relative, and never through `..`.
pub fn inside(relative: &str) -> bool {
    let path = Path::new(relative);
    !relative.is_empty()
        && path.components().all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn valid_origin(origin: &str) -> bool {
    let rest = origin.strip_prefix("https://").or_else(|| {
        origin
            .strip_prefix("http://")
            .filter(|host| host.starts_with("localhost") || host.starts_with("127.0.0.1"))
    });
    rest.is_some_and(|host| !host.is_empty() && !host.contains('/') && !host.contains('*'))
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: Self = serde_json::from_str(text).map_err(|error| format!("addin.json: {error}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), String> {
        let fail = |message: String| Err(format!("addin.json: {message}"));
        if !valid_id(&self.id) {
            return fail(format!("id {:?} must be lowercase letters and digits, joined by single dots or dashes", self.id));
        }
        if self.name.trim().is_empty() {
            return fail("name is empty".into());
        }
        if self.api == 0 || self.api > API_VERSION {
            return fail(format!("api {} is not served by this build (it serves {API_VERSION})", self.api));
        }
        if self.views.is_empty() {
            return fail("views is empty: an add-in shows at least one view".into());
        }
        let mut seen = BTreeSet::new();
        for view in &self.views {
            if !valid_name(&view.id) || !seen.insert(&view.id) {
                return fail(format!("view id {:?} is not a unique lowercase name", view.id));
            }
        }
        if !self.permissions.contains(&Permission::Network) && !self.network.is_empty() {
            return fail("network lists origins without the network permission".into());
        }
        if let Some(origin) = self.network.iter().find(|origin| !valid_origin(origin)) {
            return fail(format!("network origin {origin:?} must be https://host (or http://localhost)"));
        }
        for key in self.settings.keys() {
            if !valid_name(key) {
                return fail(format!("setting {key:?} is not a lowercase name"));
            }
        }
        let mut workers = BTreeSet::new();
        for worker in &self.workers {
            if !valid_name(&worker.id) || !workers.insert(worker.id.as_str()) {
                return fail(format!("worker id {:?} is not a unique lowercase name", worker.id));
            }
            if !worker.command.first().is_some_and(|program| inside(program)) {
                return fail(format!("worker {:?} runs a file of the add-in's folder (a relative path)", worker.id));
            }
        }
        let mut tools = BTreeSet::new();
        for tool in &self.tools {
            if !valid_name(&tool.name) || !tools.insert(tool.name.as_str()) {
                return fail(format!("tool {:?} is not a unique lowercase name", tool.name));
            }
            if !workers.contains(tool.worker.as_str()) || tool.method.is_empty() {
                return fail(format!("tool {:?} names a method of a worker this add-in declares", tool.name));
            }
        }
        let mut commands = BTreeSet::new();
        for command in &self.commands {
            if !valid_name(&command.id) || !commands.insert(&command.id) || command.title.trim().is_empty() {
                return fail(format!("command {:?} needs a unique lowercase id and a title", command.id));
            }
        }
        Ok(())
    }

    pub fn allows(&self, permission: Permission) -> bool {
        self.permissions.contains(&permission)
    }
}

/// Whose an add-in is: a project's (in its repository) or the person's own.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    Project { project_id: String },
    Personal,
}

impl Scope {
    /// Its folder name for data and trust: `personal`, or `project-<uuid>`.
    pub fn key(&self) -> String {
        match self {
            Self::Project { project_id } => format!("project-{project_id}"),
            Self::Personal => "personal".into(),
        }
    }
}

/// One add-in folder, whether or not its manifest could be read.
#[derive(Clone, Debug)]
pub struct Found {
    pub scope: Scope,
    pub dir: PathBuf,
    pub manifest: Result<Manifest, String>,
}

impl Found {
    /// Its identity across the app: `<scope key>/<id>` (the folder name when unreadable).
    pub fn key(&self) -> String {
        let id = match &self.manifest {
            Ok(manifest) => manifest.id.clone(),
            Err(_) => self.dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
        };
        format!("{}/{id}", self.scope.key())
    }
}

pub fn personal_dir(home: &Path) -> PathBuf {
    home.join("addins")
}

pub fn project_dir(root: &Path) -> PathBuf {
    root.join(PROJECT_DIR)
}

fn scan(dir: &Path, scope: &Scope, found: &mut Vec<Found>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join(MANIFEST).is_file())
        .collect();
    folders.sort();
    for folder in folders {
        let manifest = fs::read_to_string(folder.join(MANIFEST))
            .map_err(|error| format!("addin.json: {error}"))
            .and_then(|text| Manifest::parse(&text));
        found.push(Found { scope: scope.clone(), dir: folder, manifest });
    }
}

/// The add-ins a window of `project` can use: the project's, then the personal ones whose id
/// the project does not already have.
pub fn discover(home: &Path, project: Option<(&str, &Path)>) -> Vec<Found> {
    let mut found = Vec::new();
    if let Some((project_id, root)) = project {
        scan(&project_dir(root), &Scope::Project { project_id: project_id.to_owned() }, &mut found);
    }
    let mut personal = Vec::new();
    scan(&personal_dir(home), &Scope::Personal, &mut personal);
    let taken: BTreeSet<String> = found
        .iter()
        .filter_map(|addin| addin.manifest.as_ref().ok().map(|manifest| manifest.id.clone()))
        .collect();
    found.extend(personal.into_iter().filter(|addin| {
        addin.manifest.as_ref().map_or(true, |manifest| !taken.contains(&manifest.id))
    }));
    found
}

fn hex(digest: impl AsRef<[u8]>) -> String {
    digest.as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// What trusting an add-in covers: its permissions, origins, workers and tools, and every file
/// a worker runs. A change to any of them asks again; its TSX does not.
pub fn grant_digest(dir: &Path, manifest: &Manifest) -> String {
    let mut hash = Sha256::new();
    let grant = json!({
        "id": manifest.id, "permissions": manifest.permissions, "network": manifest.network,
        "workers": manifest.workers, "tools": manifest.tools,
    });
    hash.update(grant.to_string());
    for worker in &manifest.workers {
        if let Some(program) = worker.command.first() {
            hash.update(program.as_bytes());
            hash.update(fs::read(dir.join(program)).unwrap_or_default());
        }
    }
    hex(hash.finalize())
}

/// The folders a build never reads: its outputs, dependencies and version control.
const IGNORED: [&str; 5] = [".git", ".nsh", "node_modules", "out", ".DS_Store"];

/// Everything a build reads, to rebuild only when the source changed.
pub fn source_digest(dir: &Path) -> String {
    fn walk(dir: &Path, base: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if IGNORED.iter().any(|ignored| name == *ignored) {
                continue;
            }
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => walk(&path, base, files),
                Ok(kind) if kind.is_file() => files.push(path.strip_prefix(base).unwrap_or(&path).to_owned()),
                _ => {}
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    files.sort();
    let mut hash = Sha256::new();
    for file in files {
        hash.update(file.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(fs::read(dir.join(&file)).unwrap_or_default());
        hash.update([0]);
    }
    hex(hash.finalize())
}

// MARK: Trust

fn trust_path(home: &Path) -> PathBuf {
    personal_dir(home).join("trust.json")
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("no parent folder")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let staged = path.with_extension("staged");
    let mut file = fs::File::create(&staged).map_err(|error| error.to_string())?;
    file.write_all(bytes).and_then(|_| file.sync_all()).map_err(|error| error.to_string())?;
    fs::rename(&staged, path).map_err(|error| error.to_string())
}

fn read_object(path: &Path) -> Map<String, Value> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn with_trust<T>(home: &Path, edit: impl FnOnce(&mut Map<String, Value>) -> T) -> Result<T, String> {
    use fs2::FileExt;
    let dir = personal_dir(home);
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let lock = fs::File::create(dir.join("trust.lock")).map_err(|error| error.to_string())?;
    lock.lock_exclusive().map_err(|error| error.to_string())?;
    let mut grants = read_object(&trust_path(home));
    let result = edit(&mut grants);
    let bytes = serde_json::to_vec_pretty(&grants).map_err(|error| error.to_string())?;
    write_atomically(&trust_path(home), &bytes)?;
    Ok(result)
}

/// Whether the person trusted this add-in with exactly this grant.
pub fn trusted(home: &Path, key: &str, digest: &str) -> bool {
    read_object(&trust_path(home)).get(key).and_then(Value::as_str) == Some(digest)
}

pub fn trust(home: &Path, key: &str, digest: &str) -> Result<(), String> {
    with_trust(home, |grants| {
        grants.insert(key.to_owned(), json!(digest));
    })
}

pub fn revoke(home: &Path, key: &str) -> Result<(), String> {
    with_trust(home, |grants| {
        grants.remove(key);
    })
}

// MARK: Data

/// Where an add-in keeps its storage and settings: `RIWORK_HOME/addins-data/<scope>/<id>`.
pub fn data_dir(home: &Path, scope: &Scope, id: &str) -> PathBuf {
    home.join("addins-data").join(scope.key()).join(id)
}

pub fn read_storage(dir: &Path) -> Map<String, Value> {
    read_object(&dir.join("storage.json"))
}

/// Sets (or, with `None`, removes) one key of an add-in's storage, within its limit.
pub fn write_storage(dir: &Path, key: &str, value: Option<Value>) -> Result<(), String> {
    if key.is_empty() || key.len() > 200 {
        return Err("invalid: a storage key is 1 to 200 characters".into());
    }
    let mut storage = read_storage(dir);
    match value {
        Some(value) => storage.insert(key.to_owned(), value),
        None => storage.remove(key),
    };
    let bytes = serde_json::to_vec(&storage).map_err(|error| error.to_string())?;
    if bytes.len() > STORAGE_LIMIT {
        return Err("invalid: an add-in keeps at most 1 MB in its storage".into());
    }
    write_atomically(&dir.join("storage.json"), &bytes)
}

/// The add-in's settings: what the person set, else each setting's default.
pub fn settings(dir: &Path, manifest: &Manifest) -> Map<String, Value> {
    let saved = read_object(&dir.join("settings.json"));
    manifest
        .settings
        .iter()
        .map(|(key, spec)| {
            let value = saved
                .get(key)
                .filter(|value| setting_fits(spec.kind, value))
                .cloned()
                .or_else(|| spec.default.clone())
                .unwrap_or(Value::Null);
            (key.clone(), value)
        })
        .collect()
}

fn setting_fits(kind: SettingKind, value: &Value) -> bool {
    match kind {
        SettingKind::String => value.is_string(),
        SettingKind::Number => value.is_number(),
        SettingKind::Boolean => value.is_boolean(),
    }
}

pub fn set_setting(dir: &Path, manifest: &Manifest, key: &str, value: Value) -> Result<(), String> {
    let spec = manifest.settings.get(key).ok_or_else(|| format!("invalid: {} has no setting {key:?}", manifest.name))?;
    if !value.is_null() && !setting_fits(spec.kind, &value) {
        return Err(format!("invalid: {} is a {:?}", spec.label, spec.kind).to_lowercase());
    }
    let mut saved = read_object(&dir.join("settings.json"));
    if value.is_null() {
        saved.remove(key);
    } else {
        saved.insert(key.to_owned(), value);
    }
    write_atomically(&dir.join("settings.json"), &serde_json::to_vec_pretty(&saved).map_err(|error| error.to_string())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("riwork-addins-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest(extra: Value) -> String {
        let mut base = json!({"id": "acme.security-dashboard", "name": "Security", "views": [{"id": "main", "title": "Security"}]});
        base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        base.to_string()
    }

    fn write_addin(dir: &Path, folder: &str, text: &str) -> PathBuf {
        let path = dir.join(folder);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join(MANIFEST), text).unwrap();
        fs::write(path.join("App.tsx"), "export {};").unwrap();
        path
    }

    #[test]
    fn manifests_are_validated_before_anything_runs() {
        let ok = Manifest::parse(&manifest(json!({
            "permissions": ["project.read", "network"], "network": ["https://scanner.acme.internal"],
            "workers": [{"id": "scanner", "command": ["bin/scan", "--json"]}],
            "tools": [{"name": "scan_repo", "description": "Scan", "worker": "scanner", "method": "scan"}],
            "settings": {"scanner_url": {"type": "string", "label": "Scanner URL"}},
        })))
        .unwrap();
        assert!(ok.allows(Permission::ProjectRead) && !ok.allows(Permission::ShellRun));
        assert_eq!(ok.tools[0].input_schema["type"], "object");
        for (bad, why) in [
            (json!({"id": "Acme"}), "id"),
            (json!({"id": "a..b"}), "id"),
            (json!({"views": []}), "views"),
            (json!({"api": 2}), "api"),
            (json!({"permissions": ["root"]}), "unknown variant"),
            (json!({"network": ["https://x.com"]}), "network"),
            (json!({"permissions": ["network"], "network": ["http://example.com"]}), "origin"),
            (json!({"workers": [{"id": "w", "command": ["/bin/sh"]}]}), "worker"),
            (json!({"workers": [{"id": "w", "command": ["../escape"]}]}), "worker"),
            (json!({"tools": [{"name": "t", "description": "", "worker": "none", "method": "m"}]}), "tool"),
            (json!({"views": [{"id": "main", "title": "A"}, {"id": "main", "title": "B"}]}), "view"),
        ] {
            let error = Manifest::parse(&manifest(bad.clone())).unwrap_err();
            assert!(error.contains(why), "{bad}: {error}");
        }
    }

    #[test]
    fn a_project_addin_hides_a_personal_one_with_its_id() {
        let home = temp();
        let root = temp();
        write_addin(&project_dir(&root), "security", &manifest(json!({})));
        write_addin(&personal_dir(&home), "security-copy", &manifest(json!({})));
        write_addin(&personal_dir(&home), "notes", &manifest(json!({"id": "me.notes", "name": "Notes"})));
        write_addin(&personal_dir(&home), "broken", "{not json");
        let found = discover(&home, Some(("p1", &root)));
        let keys: Vec<String> = found.iter().map(Found::key).collect();
        assert_eq!(keys, ["project-p1/acme.security-dashboard", "personal/broken", "personal/me.notes"]);
        assert!(found[1].manifest.is_err());
        assert_eq!(discover(&home, None).len(), 3);
        fs::remove_dir_all(home).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn trust_follows_permissions_and_worker_files_but_not_tsx() {
        let home = temp();
        let dir = write_addin(&temp(), "a", &manifest(json!({"workers": [{"id": "w", "command": ["bin/w"]}]})));
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::write(dir.join("bin/w"), "v1").unwrap();
        let parsed = Manifest::parse(&fs::read_to_string(dir.join(MANIFEST)).unwrap()).unwrap();
        let digest = grant_digest(&dir, &parsed);
        assert!(!trusted(&home, "personal/acme.security-dashboard", &digest));
        trust(&home, "personal/acme.security-dashboard", &digest).unwrap();
        assert!(trusted(&home, "personal/acme.security-dashboard", &digest));
        fs::write(dir.join("App.tsx"), "export const changed = 1;").unwrap();
        assert_eq!(grant_digest(&dir, &parsed), digest, "TSX edits keep the grant");
        fs::write(dir.join("bin/w"), "v2").unwrap();
        assert_ne!(grant_digest(&dir, &parsed), digest, "a new worker binary asks again");
        let mut more = parsed.clone();
        more.permissions.insert(Permission::ShellRun);
        fs::write(dir.join("bin/w"), "v1").unwrap();
        assert_ne!(grant_digest(&dir, &more), digest, "a new permission asks again");
        revoke(&home, "personal/acme.security-dashboard").unwrap();
        assert!(!trusted(&home, "personal/acme.security-dashboard", &digest));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn source_digest_skips_build_outputs() {
        let dir = write_addin(&temp(), "a", &manifest(json!({})));
        let before = source_digest(&dir);
        fs::create_dir_all(dir.join("out")).unwrap();
        fs::write(dir.join("out/bundle.js"), "built").unwrap();
        assert_eq!(source_digest(&dir), before);
        fs::write(dir.join("App.tsx"), "export const x = 2;").unwrap();
        assert_ne!(source_digest(&dir), before);
    }

    #[test]
    fn storage_and_settings_stay_in_their_bounds() {
        let dir = temp();
        write_storage(&dir, "seen", Some(json!([1, 2]))).unwrap();
        assert_eq!(read_storage(&dir)["seen"], json!([1, 2]));
        write_storage(&dir, "seen", None).unwrap();
        assert!(read_storage(&dir).is_empty());
        assert!(write_storage(&dir, "big", Some(json!("x".repeat(STORAGE_LIMIT)))).is_err());
        let parsed = Manifest::parse(&manifest(json!({"settings": {
            "url": {"type": "string", "label": "URL", "default": "https://a"},
            "strict": {"type": "boolean", "label": "Strict"}}}))).unwrap();
        assert_eq!(settings(&dir, &parsed)["url"], "https://a");
        assert_eq!(settings(&dir, &parsed)["strict"], Value::Null);
        set_setting(&dir, &parsed, "strict", json!(true)).unwrap();
        assert!(set_setting(&dir, &parsed, "strict", json!("yes")).is_err());
        assert!(set_setting(&dir, &parsed, "other", json!(1)).is_err());
        assert_eq!(settings(&dir, &parsed)["strict"], true);
        fs::remove_dir_all(dir).unwrap();
    }
}
