//! UI composition, workspace mods, and shortcuts. These files are separate from
//! workspace layouts, sessions, and add-in trust. Reading never writes files.
use fs2::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const VERSION: u32 = 1;
pub const MAX_BYTES: u64 = 256 * 1024;
pub const MAX_NODES: usize = 128;
pub const MAX_DEPTH: usize = 12;
pub const MAX_SIZE: f64 = 8192.;
pub const BUILTIN_PANELS: &[&str] = &[
    "projects",
    "worktrees",
    "files",
    "preview",
    "tasks",
    "shells",
    "usage",
    "settings",
    "project_settings",
    "schedules",
    "dashboard",
    "addins",
];
const WORKSPACE_MOD_MANIFESTS: &[&str] = &[include_str!(
    "../../../apps/riwork/mods/minimalist/mod.json"
)];
pub type Bindings = BTreeMap<String, Option<String>>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub layout: String,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            version: VERSION,
            layout: "default".into(),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModsDocument {
    version: u32,
    workspace_mod: Option<String>,
}
impl Default for ModsDocument {
    fn default() -> Self {
        Self {
            version: VERSION,
            workspace_mod: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeysDocument {
    pub version: u32,
    pub bindings: Bindings,
}
impl Default for KeysDocument {
    fn default() -> Self {
        Self {
            version: VERSION,
            bindings: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModRef {
    pub addin: String,
    pub view: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandItem {
    pub command: String,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum LayoutNode {
    Row {
        id: String,
        children: Vec<LayoutNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f64>,
    },
    Column {
        id: String,
        children: Vec<LayoutNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f64>,
    },
    Workspace {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f64>,
    },
    Mod {
        id: String,
        addin: String,
        view: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f64>,
    },
    Commands {
        id: String,
        items: Vec<CommandItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f64>,
    },
    Spacer {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<f64>,
    },
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TabBar {
    #[default]
    Default,
    Hidden,
}
fn yes() -> bool {
    true
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LayoutDocument {
    pub version: u32,
    pub name: String,
    pub root: LayoutNode,
    #[serde(default = "yes")]
    pub status_bar: bool,
    #[serde(default)]
    pub tab_bar: TabBar,
    #[serde(default)]
    pub panels: BTreeMap<String, ModRef>,
}
impl Default for LayoutDocument {
    fn default() -> Self {
        Self {
            version: VERSION,
            name: "Default".into(),
            root: LayoutNode::Workspace {
                id: "workspace".into(),
                size: None,
            },
            status_bar: true,
            tab_bar: TabBar::Default,
            panels: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutInfo {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceModInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomizationSnapshot {
    pub layout: LayoutDocument,
    pub selected: String,
    pub layouts: Vec<LayoutInfo>,
    #[serde(default)]
    pub workspace_mod: Option<String>,
    #[serde(default)]
    pub mods: Vec<WorkspaceModInfo>,
    pub keys: Bindings,
    pub error: Option<String>,
    pub directory: String,
}

fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
pub fn valid_layout_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}
fn valid_node_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
impl ModRef {
    pub fn validate(&self) -> Result<(), String> {
        let valid = if let Some(id) = self.addin.strip_prefix("id:") {
            crate::addins::valid_id(id)
        } else if let Some((scope, id)) = self.addin.split_once('/') {
            (scope == "personal"
                || scope.starts_with("project-") && scope.len() > 8 && valid_node_id(scope))
                && crate::addins::valid_id(id)
        } else {
            false
        };
        if !valid {
            return Err(format!("invalid addin key: {}", self.addin));
        }
        if self.view.is_empty()
            || self.view.len() > 48
            || !self
                .view
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
        {
            return Err("mod view must be a view name from its add-in manifest".into());
        }
        if self.args.as_ref().is_some_and(|args| {
            serde_json::to_vec(args).map_or(true, |bytes| bytes.len() > 32 * 1024)
        }) {
            return Err("mod args exceed 32 KiB".into());
        }
        Ok(())
    }
}
impl LayoutDocument {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != VERSION {
            return Err(format!("unsupported layout version {}", self.version));
        }
        if !text(&self.name, 120) {
            return Err(
                "layout name must contain 1–120 characters without control characters".into(),
            );
        }
        let mut ids = BTreeSet::new();
        let mut workspaces = 0;
        fn visit(
            node: &LayoutNode,
            depth: usize,
            ids: &mut BTreeSet<String>,
            workspaces: &mut usize,
        ) -> Result<(), String> {
            if depth > MAX_DEPTH {
                return Err(format!("layout exceeds {MAX_DEPTH} nesting levels"));
            }
            let (id, size) = match node {
                LayoutNode::Row { id, size, .. }
                | LayoutNode::Column { id, size, .. }
                | LayoutNode::Workspace { id, size }
                | LayoutNode::Mod { id, size, .. }
                | LayoutNode::Commands { id, size, .. }
                | LayoutNode::Spacer { id, size } => (id, size),
            };
            if !valid_node_id(id) || !ids.insert(id.clone()) {
                return Err(format!("invalid or duplicate node id: {id}"));
            }
            if ids.len() > MAX_NODES {
                return Err(format!("layout exceeds {MAX_NODES} nodes"));
            }
            if size.is_some_and(|size| !size.is_finite() || !(1.0..=MAX_SIZE).contains(&size)) {
                return Err(format!("node {id}: size must be 1–8192 pixels"));
            }
            match node {
                LayoutNode::Row { children, .. } | LayoutNode::Column { children, .. } => {
                    if children.is_empty() || children.len() > 32 {
                        return Err(format!("node {id}: expected 1–32 children"));
                    }
                    for child in children {
                        visit(child, depth + 1, ids, workspaces)?;
                    }
                }
                LayoutNode::Workspace { .. } => *workspaces += 1,
                LayoutNode::Mod {
                    addin, view, args, ..
                } => ModRef {
                    addin: addin.clone(),
                    view: view.clone(),
                    args: args.clone(),
                }
                .validate()?,
                LayoutNode::Commands { items, .. } => {
                    if items.is_empty() || items.len() > 32 {
                        return Err(format!("node {id}: expected 1–32 commands"));
                    }
                    for item in items {
                        crate::customization_keys::validate_frame_command(&item.command)?;
                        if !text(&item.label, 80) {
                            return Err(format!(
                                "node {id}: command labels must contain 1–80 characters"
                            ));
                        }
                    }
                }
                LayoutNode::Spacer { .. } => {}
            }
            Ok(())
        }
        visit(&self.root, 0, &mut ids, &mut workspaces)?;
        if workspaces != 1 {
            return Err("layout must contain exactly one workspace leaf".into());
        }
        for (panel, module) in &self.panels {
            if !BUILTIN_PANELS.contains(&panel.as_str()) {
                return Err(format!("unknown built-in panel: {panel}"));
            }
            module.validate()?;
        }
        Ok(())
    }
}

/// Keep this store alive to retain the most recent valid configuration while an
/// editor writes a partial/invalid file. A fresh process starts with the defaults.
pub struct CustomizationStore {
    directory: PathBuf,
    last: CustomizationSnapshot,
}
impl CustomizationStore {
    pub fn open(home: impl Into<PathBuf>) -> Result<Self, String> {
        let directory = home.into().join("ui");
        let mods = WORKSPACE_MOD_MANIFESTS
            .iter()
            .map(|manifest| {
                serde_json::from_str(manifest)
                    .map_err(|error| format!("Built-in workspace mod: {error}"))
            })
            .collect::<Result<Vec<WorkspaceModInfo>, _>>()?;
        let last = CustomizationSnapshot {
            layout: LayoutDocument::default(),
            selected: "default".into(),
            layouts: vec![LayoutInfo {
                id: "default".into(),
                name: "Default".into(),
            }],
            workspace_mod: None,
            mods,
            keys: BTreeMap::new(),
            error: None,
            directory: directory.to_string_lossy().into_owned(),
        };
        Ok(Self { directory, last })
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn snapshot(&mut self) -> CustomizationSnapshot {
        let mut errors = Vec::new();
        match self.load_selected() {
            Ok((selected, layout)) => {
                self.last.selected = selected;
                self.last.layout = layout;
            }
            Err(error) => errors.push(error),
        }
        match self.load_workspace_mod() {
            Ok(workspace_mod) => self.last.workspace_mod = workspace_mod,
            Err(error) => errors.push(error),
        }
        match read_optional::<KeysDocument>(&self.directory.join("keys.json")).and_then(|keys| {
            let keys = keys.unwrap_or_default();
            if keys.version != VERSION {
                return Err(format!("keys.json: unsupported version {}", keys.version));
            }
            crate::customization_keys::normalize_bindings(&keys.bindings)
                .map_err(|error| format!("keys.json: {error}"))
        }) {
            Ok(keys) => self.last.keys = keys,
            Err(error) => errors.push(error),
        }
        let (mut layouts, catalog_errors) = self.catalog();
        errors.extend(catalog_errors);
        if !layouts.iter().any(|layout| layout.id == self.last.selected) {
            layouts.push(LayoutInfo {
                id: self.last.selected.clone(),
                name: self.last.layout.name.clone(),
            });
        }
        self.last.layouts = layouts;
        errors.sort();
        errors.dedup();
        self.last.error = (!errors.is_empty()).then(|| errors.join("\n"));
        self.last.clone()
    }
    fn load_selected(&self) -> Result<(String, LayoutDocument), String> {
        let config =
            read_optional::<Config>(&self.directory.join("config.json"))?.unwrap_or_default();
        if config.version != VERSION {
            return Err(format!(
                "config.json: unsupported version {}",
                config.version
            ));
        }
        let layout = self.load_layout(&config.layout)?;
        Ok((config.layout, layout))
    }
    fn load_workspace_mod(&self) -> Result<Option<String>, String> {
        let config =
            read_optional::<ModsDocument>(&self.directory.join("mods.json"))?.unwrap_or_default();
        if config.version != VERSION {
            return Err(format!("mods.json: unsupported version {}", config.version));
        }
        self.check_workspace_mod(config.workspace_mod.as_deref())
            .map_err(|error| format!("mods.json: {error}"))?;
        Ok(config.workspace_mod)
    }
    fn check_workspace_mod(&self, id: Option<&str>) -> Result<(), String> {
        if let Some(id) = id
            && !self.last.mods.iter().any(|module| module.id == id)
        {
            return Err(format!("Unknown workspace mod {id:?}"));
        }
        Ok(())
    }
    fn load_layout(&self, name: &str) -> Result<LayoutDocument, String> {
        if !valid_layout_id(name) {
            return Err(
                "layout must be a file name containing lowercase letters, digits, '-' or '_'"
                    .into(),
            );
        }
        if name == "default" {
            return Ok(LayoutDocument::default());
        }
        let path = self.directory.join("layouts").join(format!("{name}.json"));
        let layout: LayoutDocument =
            read_optional(&path)?.ok_or_else(|| format!("layout {name}.json does not exist"))?;
        layout
            .validate()
            .map_err(|error| format!("{name}.json: {error}"))?;
        Ok(layout)
    }
    fn catalog(&self) -> (Vec<LayoutInfo>, Vec<String>) {
        let mut layouts = vec![LayoutInfo {
            id: "default".into(),
            name: "Default".into(),
        }];
        let mut errors = Vec::new();
        let entries = match fs::read_dir(self.directory.join("layouts")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (layouts, errors),
            Err(error) => return (layouts, vec![format!("Cannot read UI layouts: {error}")]),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    errors.push(error.to_string());
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            if name == "default" {
                continue;
            }
            names.push(name.to_owned());
            if names.len() > 128 {
                errors.push("UI layouts directory exceeds 128 layouts".into());
                break;
            }
        }
        names.sort();
        for name in names {
            match self.load_layout(&name) {
                Ok(layout) => layouts.push(LayoutInfo {
                    id: name,
                    name: layout.name,
                }),
                Err(error) => errors.push(error),
            }
        }
        (layouts, errors)
    }
    pub fn select(&mut self, name: &str) -> Result<CustomizationSnapshot, String> {
        self.load_layout(name)?;
        let _lock = self.write_lock()?;
        write_json(
            &self.directory.join("config.json"),
            &Config {
                version: VERSION,
                layout: name.into(),
            },
        )?;
        Ok(self.snapshot())
    }
    pub fn set_workspace_mod(&mut self, id: Option<&str>) -> Result<CustomizationSnapshot, String> {
        self.check_workspace_mod(id)?;
        let _lock = self.write_lock()?;
        write_json(
            &self.directory.join("mods.json"),
            &ModsDocument {
                version: VERSION,
                workspace_mod: id.map(str::to_owned),
            },
        )?;
        Ok(self.snapshot())
    }
    pub fn reset(&mut self) -> Result<CustomizationSnapshot, String> {
        let _lock = self.write_lock()?;
        write_json(&self.directory.join("config.json"), &Config::default())?;
        write_json(&self.directory.join("mods.json"), &ModsDocument::default())?;
        let keys = self.directory.join("keys.json");
        match fs::symlink_metadata(&keys) {
            Ok(_) => {
                let archive = self.directory.join("archive");
                fs::create_dir_all(&archive).map_err(|e| e.to_string())?;
                fs::rename(
                    &keys,
                    archive.join(format!("keys-{}.json", uuid::Uuid::new_v4())),
                )
                .map_err(|e| format!("Cannot archive shortcuts: {e}"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        Ok(self.snapshot())
    }
    pub fn initialize_files(&mut self) -> Result<CustomizationSnapshot, String> {
        let _lock = self.write_lock()?;
        fs::create_dir_all(self.directory.join("layouts")).map_err(|e| e.to_string())?;
        create_once(
            &self.directory.join("config.json"),
            &serde_json::to_vec_pretty(&Config::default()).unwrap(),
        )?;
        create_once(
            &self.directory.join("keys.json"),
            &serde_json::to_vec_pretty(&KeysDocument::default()).unwrap(),
        )?;
        create_once(
            &self.directory.join("layouts/sidebar.json"),
            &serde_json::to_vec_pretty(&sidebar_layout()).unwrap(),
        )?;
        create_once(&self.directory.join("README.md"), STARTER_GUIDE.as_bytes())?;
        Ok(self.snapshot())
    }
    fn write_lock(&self) -> Result<File, String> {
        fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join(".lock"))
            .map_err(|e| e.to_string())?;
        file.lock_exclusive().map_err(|e| e.to_string())?;
        Ok(file)
    }
}
fn read_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(format!(
            "{} must be a regular JSON file smaller than 256 KiB",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("{} exceeds 256 KiB", path.display()));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("{}: {error}", path.display()))
}
fn create_once(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let temp = path.with_file_name(format!(".ui-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        create_once(&temp, &bytes)?;
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        File::open(path.parent().ok_or("invalid UI directory")?)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
pub fn sidebar_layout() -> LayoutDocument {
    serde_json::from_value(json!({"version":1,"name":"Sidebar","root":{"type":"row","id":"main","children":[{"type":"commands","id":"shortcuts","size":180,"items":[{"command":"newShell","label":"New shell"},{"command":"newChat","label":"New chat"},{"command":"openFiles","label":"Files"},{"command":"openTasks","label":"Tasks"}]},{"type":"workspace","id":"workspace"}]},"statusBar":true,"tabBar":"default"})).expect("built-in sidebar schema")
}
const STARTER_GUIDE: &str = r#"# Somtus UI customization

These files customize presentation and shortcuts. They do not replace saved workspace layouts, sessions, or add-in permissions.

- `config.json`: `{ "version": 1, "layout": "sidebar" }` selects `layouts/sidebar.json`.
- `layouts/*.json`: a named layout; copy the sidebar example and give the file a new lowercase name.
- `mods.json`: the workspace mod selected in Settings → Mods.
- `keys.json`: `{ "version": 1, "bindings": { "newChat": "mod+shift+k" } }`; use `null` to unbind a supported command. Unsupported or conflicting chords are reported and the last valid keymap stays active.
- `default` is reserved for the original interface. `somtus customization reset` selects it, turns off the workspace mod, and archives keys in `archive/`. It leaves custom layouts, workspace layouts, add-ins, and sessions intact.

Run `somtus customization init` to create missing examples, `somtus customization select sidebar` to use the sidebar, and `somtus customization show --json` to inspect the effective configuration and any errors. Files reload automatically. Invalid edits retain the last valid configuration in a running app; a fresh app starts with the safe default if no valid configuration is available.

Layout schema: `{ "version": 1, "name": "My layout", "root": NODE, "statusBar": true, "tabBar": "default", "panels": {} }`. `tabBar` can also be `hidden`. Exactly one workspace leaf is required. Every node has a unique `id`; optional `size` is 1–8192 pixels along its parent's axis. Nodes without a size share the remaining space. Containers cannot be empty; layouts allow at most 128 nodes and 12 nesting levels. JSON files are limited to 256 KiB. Unknown fields are errors.

Node types:
- `row` / `column`: `children: [NODE, ...]`.
- `workspace`: the existing workspace and all its live sessions.
- `commands`: `items: [{ "command": "openFiles", "label": "Files" }]`.
- `spacer`: empty space.
- `mod`: `addin: "personal/my.addin", view: "main", args: {}`. Use an installed add-in's exact key and exported view, or the portable alias `id:<manifest.id>` such as `id:example.workspace-navigator`. Existing add-in trust and permission checks still apply.

`panels` optionally replaces a built-in panel with a mod reference, e.g. `"files": { "addin": "personal/my.addin", "view": "main" }`. Supported panel IDs: projects, worktrees, files, preview, tasks, shells, usage, settings, project_settings, schedules, dashboard, addins. A replacement does not grant additional add-in permissions.
"#;

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("somtus-customization-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn store(&self) -> CustomizationStore {
            CustomizationStore::open(&self.0).unwrap()
        }
        fn write(&self, relative: &str, content: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn parse(value: Value) -> Result<LayoutDocument, String> {
        let layout: LayoutDocument = serde_json::from_value(value).map_err(|e| e.to_string())?;
        layout.validate()?;
        Ok(layout)
    }
    #[test]
    fn customization_missing_files_use_original_ui_without_writing_anything() {
        let temp = Temp::new();
        let snapshot = temp.store().snapshot();
        assert_eq!(snapshot.layout, LayoutDocument::default());
        assert_eq!(snapshot.selected, "default");
        assert!(snapshot.keys.is_empty() && snapshot.error.is_none());
        assert!(!temp.0.join("ui").exists());
    }
    #[test]
    fn customization_workspace_mod_ships_separately_from_layouts_and_persists() {
        let temp = Temp::new();
        let mut store = temp.store();
        let fresh = store.snapshot();
        assert_eq!(fresh.workspace_mod, None);
        let module = fresh
            .mods
            .iter()
            .find(|module| module.id == "somtus.minimalist")
            .unwrap();
        assert_eq!(module.name, "Minimalist");
        assert!(
            !fresh
                .layouts
                .iter()
                .any(|layout| layout.name == module.name)
        );
        assert!(!temp.0.join("ui").exists());

        let enabled = store.set_workspace_mod(Some(&module.id)).unwrap();
        assert_eq!(enabled.workspace_mod.as_deref(), Some(module.id.as_str()));
        assert_eq!(enabled.layout, fresh.layout);
        assert_eq!(enabled.selected, fresh.selected);
        assert_eq!(temp.store().snapshot().workspace_mod, enabled.workspace_mod);

        store.initialize_files().unwrap();
        assert_eq!(
            store.select("sidebar").unwrap().workspace_mod,
            enabled.workspace_mod
        );
        let disabled = store.set_workspace_mod(None).unwrap();
        assert_eq!(disabled.workspace_mod, None);
        assert_eq!(disabled.selected, "sidebar");
        assert_eq!(temp.store().snapshot().workspace_mod, None);

        store.set_workspace_mod(Some(&module.id)).unwrap();
        let reset = store.reset().unwrap();
        assert_eq!(reset.workspace_mod, None);
        assert_eq!(reset.selected, "default");
        assert_eq!(temp.store().snapshot().workspace_mod, None);
    }
    #[test]
    fn customization_workspace_mod_invalid_edits_keep_last_good_selection() {
        let temp = Temp::new();
        let mut store = temp.store();
        let enabled = store.set_workspace_mod(Some("somtus.minimalist")).unwrap();
        assert!(store.set_workspace_mod(Some("unknown.mod")).is_err());
        assert_eq!(temp.store().snapshot().workspace_mod, enabled.workspace_mod);

        for invalid in [
            "{ incomplete",
            r#"{"version":2}"#,
            r#"{"version":1,"workspaceMod":"unknown.mod"}"#,
        ] {
            temp.write("ui/mods.json", invalid);
            let current = store.snapshot();
            assert_eq!(current.workspace_mod, enabled.workspace_mod);
            assert!(current.error.unwrap().contains("mods.json"));
            assert_eq!(temp.store().snapshot().workspace_mod, None);
        }
        temp.write("ui/mods.json", r#"{"version":1,"workspaceMod":null}"#);
        let recovered = store.snapshot();
        assert_eq!(recovered.workspace_mod, None);
        assert_eq!(recovered.error, None);
    }
    #[test]
    fn customization_rejects_unknown_fields_versions_duplicates_and_workspace_counts() {
        let base = serde_json::to_value(sidebar_layout()).unwrap();
        parse(base.clone()).unwrap();
        for patch in [json!({"version":2}), json!({"unknown":true})] {
            let mut value = base.clone();
            value
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(parse(value).is_err());
        }
        for root in [
            json!({"type":"workspace","id":"workspace","children":[]}),
            json!({"type":"unknown","id":"workspace"}),
            json!({"type":"spacer","id":"nothing"}),
            json!({"type":"row","id":"main","children":[]}),
            json!({"type":"row","id":"main","children":[{"type":"workspace","id":"one"},{"type":"workspace","id":"two"}]}),
            json!({"type":"row","id":"main","children":[{"type":"workspace","id":"same"},{"type":"spacer","id":"same"}]}),
            json!({"type":"row","id":"main","children":[{"type":"workspace","id":"workspace"},{"type":"commands","id":"cmds","items":[{"command":"runAnything","label":"Run"}]}]}),
        ] {
            let mut value = base.clone();
            value["root"] = root;
            assert!(parse(value).is_err());
        }
        for command in ["quit", "openGlobalDashboard", "biggerText", "selectTab"] {
            let mut value = base.clone();
            value["root"]["children"][0]["items"][0]["command"] = json!(command);
            assert!(parse(value).is_err(), "command block must reject {command}");
        }
    }
    #[test]
    fn customization_bounds_sizes_depth_nodes_mods_and_builtin_panels() {
        let base = serde_json::to_value(LayoutDocument::default()).unwrap();
        for size in [0., -1., 8193.] {
            let mut value = base.clone();
            value["root"]["size"] = json!(size);
            assert!(parse(value).is_err());
        }
        let mut deep = base.clone();
        for n in 0..MAX_DEPTH + 1 {
            deep["root"] = json!({"type":"column","id":format!("level-{n}"),"children":[deep["root"].clone()]});
        }
        assert!(parse(deep).unwrap_err().contains("nesting"));
        let children:Vec<_>=(0..32).map(|group|json!({"type":"row","id":format!("group-{group}"),"children":(0..4).map(|n|json!({"type":"spacer","id":format!("spacer-{group}-{n}")})).collect::<Vec<_>>()})).collect();
        let mut huge = base.clone();
        huge["root"] = json!({"type":"column","id":"root","children":children});
        assert!(parse(huge).unwrap_err().contains("nodes"));
        let mut bad = base.clone();
        bad["panels"] = json!({"unknown":{"addin":"personal/test","view":"main"}});
        assert!(parse(bad).is_err());
        for addin in [
            "../bad",
            "personal/../bad",
            "project-/test",
            "test",
            "id:../bad",
            "id:",
        ] {
            assert!(
                ModRef {
                    addin: addin.into(),
                    view: "main".into(),
                    args: None
                }
                .validate()
                .is_err()
            );
        }
        ModRef {
            addin: "id:example.workspace-navigator".into(),
            view: "main".into(),
            args: None,
        }
        .validate()
        .unwrap();
        let mut mods = base;
        mods["panels"] =
            json!({"files":{"addin":"personal/my.addin","view":"main","args":{"hello":"world"}}});
        parse(mods).unwrap();
    }
    #[test]
    fn customization_invalid_edits_retain_last_good_components_and_recover() {
        let temp = Temp::new();
        let mut store = temp.store();
        store.initialize_files().unwrap();
        temp.write(
            "ui/keys.json",
            r#"{"version":1,"bindings":{"newChat":null}}"#,
        );
        let good = store.select("sidebar").unwrap();
        assert_eq!(good.keys.get("newChat"), Some(&None));
        temp.write("ui/layouts/sidebar.json", "{ incomplete");
        temp.write(
            "ui/keys.json",
            r#"{"version":1,"bindings":{"notACommand":"mod+k"}}"#,
        );
        let invalid = store.snapshot();
        assert_eq!(invalid.layout, good.layout);
        assert_eq!(invalid.keys, good.keys);
        assert_eq!(invalid.selected, "sidebar");
        assert!(invalid.error.is_some());
        temp.write(
            "ui/layouts/sidebar.json",
            &serde_json::to_string(&sidebar_layout()).unwrap(),
        );
        temp.write("ui/keys.json", r#"{"version":1,"bindings":{}}"#);
        assert!(store.snapshot().error.is_none());
        temp.write(
            "ui/config.json",
            r#"{"version":1,"layout":"../../outside"}"#,
        );
        let invalid = store.snapshot();
        assert_eq!(invalid.selected, "sidebar");
        assert!(invalid.error.is_some());
    }
    #[test]
    fn customization_initialize_preserves_edits_and_reset_archives_only_keys() {
        let temp = Temp::new();
        let mut store = temp.store();
        store.initialize_files().unwrap();
        store.select("sidebar").unwrap();
        let keys = r#"{"version":1,"bindings":{"newChat":null}}"#;
        temp.write("ui/keys.json", keys);
        temp.write("ui/README.md", "my guide");
        temp.write("layouts.json", "original workspace");
        temp.write("addins/marker", "original addins");
        temp.write("sessions/marker", "original sessions");
        store.initialize_files().unwrap();
        assert_eq!(
            fs::read_to_string(temp.0.join("ui/keys.json")).unwrap(),
            keys
        );
        assert_eq!(
            fs::read_to_string(temp.0.join("ui/README.md")).unwrap(),
            "my guide"
        );
        temp.write("ui/layouts/broken.json", "{ invalid inactive layout");
        let reset = store.reset().unwrap();
        assert_eq!(reset.selected, "default");
        assert!(reset.keys.is_empty());
        assert!(
            reset
                .error
                .as_deref()
                .is_some_and(|error| error.contains("broken.json"))
        );
        let archived = fs::read_dir(temp.0.join("ui/archive"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::read_to_string(archived).unwrap(), keys);
        assert!(temp.0.join("ui/layouts/sidebar.json").exists());
        for (file, expected) in [
            ("layouts.json", "original workspace"),
            ("addins/marker", "original addins"),
            ("sessions/marker", "original sessions"),
        ] {
            assert_eq!(fs::read_to_string(temp.0.join(file)).unwrap(), expected);
        }
    }
    #[test]
    fn customization_failed_selection_never_rewrites_config_and_large_files_are_bounded() {
        let temp = Temp::new();
        let mut store = temp.store();
        store.initialize_files().unwrap();
        let before = fs::read(temp.0.join("ui/config.json")).unwrap();
        assert!(store.select("../invalid").is_err());
        assert!(store.select("missing").is_err());
        assert_eq!(fs::read(temp.0.join("ui/config.json")).unwrap(), before);
        temp.write("ui/keys.json", &" ".repeat(MAX_BYTES as usize + 1));
        assert!(store.snapshot().error.unwrap().contains("256 KiB"));
        temp.write("ui/config.json", r#"{"version":2,"layout":"default"}"#);
        assert_eq!(store.snapshot().selected, "default");
    }
}
