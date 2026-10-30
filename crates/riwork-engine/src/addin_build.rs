//! Builds an add-in's TSX into an NSH bundle with the NSH CLI (0.3.0, as `riwork update` uses).
//!
//! The author's folder is copied into a staging folder; RiWork writes its `nsh.json` there
//! (the add-in's NSH id, and capabilities that allow exactly the add-in API's host calls and the
//! network origins its manifest names, nothing else), and the current `addin-api.ts`. An
//! `nsh.json` of the author's own is ignored. A folder that declares a Rust crate is refused, so
//! an add-in never reaches RiWork's native modules. Builds are cached by the source's digest.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use riwork_core::addins::{self, Manifest, Permission, Scope};
use serde_json::json;

/// The add-in API's TypeScript, written into every add-in that is built.
pub const SDK: &str = include_str!("../addin/addin-api.ts");
pub const SDK_FILE: &str = "addin-api.ts";
const NSH_VERSION: &str = "0.3.0";

/// The NSH id of an add-in's bundle: its data and storage are kept under it.
pub fn nsh_id(scope: &Scope, id: &str) -> String {
    let scope = match scope {
        Scope::Personal => "personal".to_owned(),
        Scope::Project { project_id } => format!("p{}", project_id.replace('-', "")),
    };
    format!("dev.riwork.addin.{scope}.{}", id.replace(['.', '-'], "_"))
}

/// The NSH CLI: `NSH_CLI` (absolute), RiWork's toolchain folder, then `nsh` on PATH; it must
/// be version 0.3.0, as RiWork's own UI build.
pub fn find_nsh(home: &Path) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("NSH_CLI").filter(|path| !path.is_empty()) {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(home.join("toolchains/nsh-0.3.0/bin/nsh"));
    if let Some(path) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(path).join(".local/share/riwork/toolchains/nsh-0.3.0/bin/nsh"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("nsh")));
    }
    let mut seen = Vec::new();
    for candidate in candidates.into_iter().filter(|path| path.is_absolute() && path.is_file()) {
        let version = Command::new(&candidate).arg("--version").output().ok();
        let text = version.map(|output| String::from_utf8_lossy(&output.stdout).into_owned()).unwrap_or_default();
        if text.split_whitespace().nth(1) == Some(NSH_VERSION) {
            return Ok(candidate);
        }
        seen.push(format!("{} ({})", candidate.display(), text.trim()));
    }
    Err(format!(
        "Building add-ins needs the NSH CLI {NSH_VERSION}; set NSH_CLI to its path.{}",
        if seen.is_empty() { String::new() } else { format!(" Found: {}", seen.join(", ")) }
    ))
}

/// The capabilities an add-in's bundle gets: the add-in API, and the network origins its
/// manifest names when it has `network`. Nothing else: no processes, files, clipboard or
/// pickers.
pub fn capabilities(manifest: &Manifest) -> serde_json::Value {
    let http: Vec<_> = if manifest.allows(Permission::Network) {
        manifest
            .network
            .iter()
            .filter_map(|origin| {
                let (scheme, host) = origin.split_once("://")?;
                Some(json!({"scheme": scheme, "host": host}))
            })
            .collect()
    } else {
        Vec::new()
    };
    json!({"host": ["addin.*"], "network": {"http": http}})
}

fn copy_sources(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if [".git", ".nsh", "node_modules", "out", "nsh.json", "Cargo.toml", ".DS_Store"].contains(&name_text.as_ref()) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_sources(&entry.path(), &to.join(&name))?;
        } else if kind.is_file() {
            fs::copy(entry.path(), to.join(&name))?;
        }
    }
    Ok(())
}

/// A built add-in: its bundle, and the digest of the source it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Built {
    pub bundle: PathBuf,
    pub digest: String,
}

const TEMPLATE_MANIFEST: &str = include_str!("../addin/template/addin.json");
const TEMPLATE_APP: &str = include_str!("../addin/template/App.tsx");

/// An add-in id from a name: `Security Dashboard` is `security-dashboard`.
pub fn id_from_name(name: &str) -> String {
    let mut id = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            id.push(c);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    id.trim_end_matches('-').chars().take(64).collect()
}

/// A new add-in from the template in `parent/<id>`: its manifest, `App.tsx`, and the current
/// `addin-api.ts` for editors. Refuses a folder that exists.
pub fn scaffold(parent: &Path, id: &str, name: &str) -> Result<PathBuf, String> {
    if !addins::valid_id(id) {
        return Err(format!("invalid: {id:?} is not an add-in id (lowercase letters and digits, joined by dots or dashes)"));
    }
    let name = name.trim();
    if name.is_empty() || name.len() > 80 || name.contains(['"', '\\', '\n']) {
        return Err("invalid: an add-in name is 1 to 80 characters without quotes or backslashes".into());
    }
    let dir = parent.join(id.rsplit('.').next().unwrap_or(id));
    if dir.exists() {
        return Err(format!("{} exists already", dir.display()));
    }
    let fill = |text: &str| text.replace("{{id}}", id).replace("{{name}}", name);
    fs::create_dir_all(&dir)
        .and_then(|_| fs::write(dir.join(addins::MANIFEST), fill(TEMPLATE_MANIFEST)))
        .and_then(|_| fs::write(dir.join("App.tsx"), fill(TEMPLATE_APP)))
        .and_then(|_| fs::write(dir.join(SDK_FILE), SDK))
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    Ok(dir)
}

/// Where builds of an add-in are kept: `RIWORK_HOME/addins-cache/<scope>/<id>/<digest>`.
pub fn cache_dir(home: &Path, scope: &Scope, id: &str) -> PathBuf {
    home.join("addins-cache").join(scope.key()).join(id)
}

/// Builds the add-in in `dir` unless this source was built already.
pub fn build(home: &Path, scope: &Scope, dir: &Path, manifest: &Manifest) -> Result<Built, String> {
    if dir.join("Cargo.toml").exists() {
        return Err("Add-ins are TSX only: this folder has a Cargo.toml. Native code goes in a worker.".into());
    }
    if !dir.join("App.tsx").is_file() {
        return Err("The add-in has no App.tsx.".into());
    }
    let digest = addins::source_digest(dir);
    let cache = cache_dir(home, scope, &manifest.id);
    let out = cache.join(&digest);
    let bundle = out.join("bundle.js");
    if bundle.is_file() {
        return Ok(Built { bundle, digest });
    }
    let nsh = find_nsh(home)?;
    let staging = cache.join(format!(".staging-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        copy_sources(dir, &staging).map_err(|error| format!("could not stage the add-in: {error}"))?;
        let nsh_json = json!({
            "name": manifest.name, "id": nsh_id(scope, &manifest.id),
            "version": if manifest.version.is_empty() { "0.0.0" } else { manifest.version.as_str() },
            "capabilities": capabilities(manifest),
        });
        fs::write(staging.join("nsh.json"), serde_json::to_vec_pretty(&nsh_json).unwrap())
            .and_then(|_| fs::write(staging.join(SDK_FILE), SDK))
            .map_err(|error| format!("could not stage the add-in: {error}"))?;
        let built = staging.join(".nsh-out");
        let output = Command::new(&nsh)
            .arg("build")
            .arg(&staging)
            .arg("--out")
            .arg(&built)
            .env_remove("NSH_CLI")
            .output()
            .map_err(|error| format!("could not run the NSH CLI: {error}"))?;
        if !output.status.success() || !built.join("bundle.js").is_file() {
            let text = String::from_utf8_lossy(&output.stderr).into_owned() + &String::from_utf8_lossy(&output.stdout);
            let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty() && !line.starts_with("warn")).collect();
            return Err(lines[lines.len().saturating_sub(12)..].join("\n"));
        }
        if out.exists() {
            fs::remove_dir_all(&out).ok();
        }
        fs::rename(&built, &out).map_err(|error| format!("could not keep the build: {error}"))
    })();
    fs::remove_dir_all(&staging).ok();
    result?;
    // Keep the newest few builds: an editor saving often must not fill the disk.
    prune(&cache, &digest);
    Ok(Built { bundle, digest })
}

fn prune(cache: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(cache) else { return };
    let mut builds: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.') && entry.file_name() != keep)
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    builds.sort();
    let excess = builds.len().saturating_sub(3);
    for (_, path) in builds.into_iter().take(excess) {
        fs::remove_dir_all(path).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(extra: serde_json::Value) -> Manifest {
        let mut base = json!({"id": "acme.scan", "name": "Scan", "views": [{"id": "main", "title": "Scan"}]});
        base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        Manifest::parse(&base.to_string()).unwrap()
    }

    #[test]
    fn capabilities_allow_the_addin_api_and_named_origins_only() {
        let none = capabilities(&manifest(json!({})));
        assert_eq!(none, json!({"host": ["addin.*"], "network": {"http": []}}));
        let net = capabilities(&manifest(json!({"permissions": ["network"], "network": ["https://scan.acme.dev", "http://localhost:8080"]})));
        assert_eq!(net["network"]["http"], json!([{"scheme": "https", "host": "scan.acme.dev"}, {"scheme": "http", "host": "localhost:8080"}]));
        for absent in ["process", "fs", "clipboard", "picker", "webview", "openUrl"] {
            assert!(net.get(absent).is_none(), "{absent}");
        }
    }

    #[test]
    fn nsh_ids_keep_scopes_apart() {
        let project = Scope::Project { project_id: "6f1c-22".into() };
        assert_eq!(nsh_id(&project, "acme.scan-x"), "dev.riwork.addin.p6f1c22.acme_scan_x");
        assert_eq!(nsh_id(&Scope::Personal, "acme.scan-x"), "dev.riwork.addin.personal.acme_scan_x");
    }

    /// The template, through the real NSH CLI: `cargo test -p riwork-engine --lib -- --ignored
    /// builds_the_template` (needs NSH 0.3.0, found as `find_nsh` does).
    #[test]
    #[ignore = "runs the NSH CLI"]
    fn builds_the_template_with_the_nsh_cli() {
        let root = std::env::temp_dir().join(format!("riwork-addin-template-{}", uuid::Uuid::new_v4()));
        let dir = root.join("addin");
        fs::create_dir_all(&dir).unwrap();
        let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("addin/template");
        for file in ["addin.json", "App.tsx"] {
            let text = fs::read_to_string(template.join(file)).unwrap();
            fs::write(dir.join(file), text.replace("{{id}}", "me.template").replace("{{name}}", "Template")).unwrap();
        }
        let manifest = Manifest::parse(&fs::read_to_string(dir.join("addin.json")).unwrap()).unwrap();
        let home = root.join("home");
        let built = build(&home, &Scope::Personal, &dir, &manifest).unwrap();
        assert!(fs::metadata(&built.bundle).unwrap().len() > 1000);
        let capabilities: serde_json::Value =
            serde_json::from_slice(&fs::read(built.bundle.with_file_name("capabilities.json")).unwrap()).unwrap();
        assert_eq!(capabilities["host"], json!(["addin.*"]), "{capabilities}");
        // The same source is not built again.
        assert_eq!(build(&home, &Scope::Personal, &dir, &manifest).unwrap(), built);
        fs::remove_dir_all(root).unwrap();
    }

    /// The example add-in in examples/addins, through the real NSH CLI.
    #[test]
    #[ignore = "runs the NSH CLI"]
    fn builds_the_example_addins() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/addins");
        let home = std::env::temp_dir().join(format!("riwork-addin-examples-{}", uuid::Uuid::new_v4()));
        for entry in fs::read_dir(&examples).unwrap().flatten() {
            let manifest = Manifest::parse(&fs::read_to_string(entry.path().join("addin.json")).unwrap()).unwrap();
            let built = build(&home, &Scope::Personal, &entry.path(), &manifest)
                .unwrap_or_else(|error| panic!("{}: {error}", entry.path().display()));
            assert!(built.bundle.is_file());
        }
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn scaffolds_a_valid_addin_once() {
        let parent = std::env::temp_dir().join(format!("riwork-addin-new-{}", uuid::Uuid::new_v4()));
        assert_eq!(id_from_name("  Security Dashboard!! "), "security-dashboard");
        let dir = scaffold(&parent, "acme.security-dashboard", "Security Dashboard").unwrap();
        assert!(dir.ends_with("security-dashboard"));
        let manifest = Manifest::parse(&fs::read_to_string(dir.join("addin.json")).unwrap()).unwrap();
        assert_eq!((manifest.id.as_str(), manifest.name.as_str()), ("acme.security-dashboard", "Security Dashboard"));
        assert_eq!(fs::read_to_string(dir.join(SDK_FILE)).unwrap(), SDK);
        assert!(scaffold(&parent, "acme.security-dashboard", "Again").unwrap_err().contains("exists"));
        assert!(scaffold(&parent, "Bad Id", "X").is_err());
        assert!(scaffold(&parent, "ok", "with \"quote").is_err());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn a_rust_crate_or_missing_entry_is_refused_before_any_build() {
        let dir = std::env::temp_dir().join(format!("riwork-addin-build-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let home = dir.join("home");
        assert!(build(&home, &Scope::Personal, &dir, &manifest(json!({}))).unwrap_err().contains("App.tsx"));
        fs::write(dir.join("App.tsx"), "").unwrap();
        fs::write(dir.join("Cargo.toml"), "").unwrap();
        assert!(build(&home, &Scope::Personal, &dir, &manifest(json!({}))).unwrap_err().contains("Cargo.toml"));
        fs::remove_dir_all(dir).unwrap();
    }
}
