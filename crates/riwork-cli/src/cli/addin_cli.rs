//! `somtus addin`: the add-ins of this folder's project and the person's own, made from the
//! template, trusted or not, and their agent tools run as `somtus mcp` runs them.

use std::path::{Path, PathBuf};

use riwork_core::addins::{self, Found, Manifest, Scope};
use riwork_engine::addin_build;
use serde_json::{Value, json};

use super::{print_json, take_flag, take_option, write_stdout};
use crate::mcp;

const HELP: &str = "\
somtus addin list [--json]              The add-ins of this folder's project, and your personal ones
somtus addin new NAME [--personal] [--id ID] [--json]   Make an add-in from the template
somtus addin trust KEY [--json]         Show what an add-in asks for, and grant it
somtus addin revoke KEY [--json]        Take that grant back
somtus addin tool KEY TOOL [JSON] [--json]   Run an add-in's agent tool as somtus mcp does
somtus addin pack|install|catalog       Not available yet

An add-in is a folder with an addin.json manifest and NSH TSX sources (App.tsx), and
optionally workers: programs it ships, run as processes of their own, which speak JSON
lines on stdin and stdout. A project's add-ins live in <project root>/.ontack/addins/<folder>
and travel with its repository; personal ones live in RIWORK_HOME/addins/<folder>, for
every project. The project is the one this Somtus session belongs to, else the one whose
folder holds the current directory. new makes the add-in there (personal with --personal),
its id from NAME unless --id says it, and refuses a folder that exists.
KEY is <scope>/<id> as list shows it (personal/me.notes, project-<uuid>/acme.scan), or the
id alone. Nothing of an add-in runs until it is trusted. trust grants its permissions,
network origins, workers and tools, and the files its workers run; a change to any of
them asks for trust again, while its TSX may change freely. While it is trusted, somtus mcp
offers agents its tools as addin__<id>__<tool>, the id's dots and dashes as _ (a name over
64 characters, or a tool whose input_schema is not an object's, is left out; list says so).
tool takes the tool's arguments as one JSON object (default {}), runs its worker's method
with 120 s to answer, and prints the result as JSON. A worker's stderr goes to
RIWORK_HOME/addins-data/<scope>/<id>/logs/<worker>.log.
--json prints {\"v\":1,...}, or {\"error\":{\"code\",\"message\"}} with exit status 2.
";

type Refusal = (&'static str, String);

fn invalid(message: impl Into<String>) -> Refusal {
    ("invalid_request", message.into())
}

fn internal(message: String) -> Refusal {
    ("cli_error", message)
}

pub(super) fn command(args: Vec<String>, json: bool) -> Result<(), String> {
    if args.is_empty()
        || matches!(args.as_slice(), [help] if matches!(help.as_str(), "help" | "--help" | "-h"))
    {
        print!("{HELP}");
        return Ok(());
    }
    match result(args) {
        Ok((value, _)) if json => print_json(&value),
        Ok((_, text)) => {
            print!("{text}");
            Ok(())
        }
        Err((code, message)) if json => {
            print_json(&json!({"error":{"code":code,"message":message}}))?;
            Err(String::new())
        }
        Err((_, message)) => Err(message),
    }
}

/// What the command prints: with --json, and as text.
fn result(mut args: Vec<String>) -> Result<(Value, String), Refusal> {
    let operation = args.remove(0);
    let home = crate::paths::riwork_home().map_err(internal)?;
    let project = std::env::current_dir()
        .ok()
        .and_then(|cwd| mcp::addin_project(&home, &cwd));
    let project = mcp::project_ref(&project);
    let single = |args: &[String], usage: &str| match args {
        [key] if !key.starts_with("--") => Ok(key.clone()),
        _ => Err(invalid(format!("Usage: somtus addin {usage}"))),
    };
    match operation.as_str() {
        "list" if args.is_empty() => Ok(list(&home, project)),
        "new" => new(&home, project, args),
        "trust" => trust(&home, project, &single(&args, "trust KEY")?),
        "revoke" => revoke(&home, project, &single(&args, "revoke KEY")?),
        "tool" => tool(&home, project, args),
        // Packaging (riwork_engine::addin_package) is being written; it plugs in here.
        "pack" | "install" | "catalog" => Err((
            "not_available",
            format!("somtus addin {operation} is not available yet"),
        )),
        _ => Err(invalid(
            "Usage: somtus addin list | new NAME | trust KEY | revoke KEY | tool KEY TOOL [JSON]; run somtus addin help",
        )),
    }
}

fn scope_name(scope: &Scope) -> &'static str {
    match scope {
        Scope::Project { .. } => "project",
        Scope::Personal => "personal",
    }
}

/// The add-in KEY names: by its key, or by its id (its folder, for one that cannot be read).
fn find(home: &Path, project: Option<(&str, &Path)>, key: &str) -> Result<Found, Refusal> {
    addins::discover(home, project)
        .into_iter()
        .find(|addin| {
            let own = addin.key();
            own == key || own.rsplit('/').next() == Some(key)
        })
        .ok_or_else(|| {
            (
                "not_found",
                format!("There is no add-in {key} here; run somtus addin list"),
            )
        })
}

fn unreadable(addin: &Found, error: &str) -> Refusal {
    invalid(format!("{} cannot be used: {error}", addin.key()))
}

fn trusted(home: &Path, addin: &Found, manifest: &Manifest) -> bool {
    addins::trusted(
        home,
        &addin.key(),
        &addins::grant_digest(&addin.dir, manifest),
    )
}

/// Strings of a JSON array, joined for text.
fn joined(values: &Value) -> String {
    let names: Vec<&str> = values
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    names.join(", ")
}

fn list(home: &Path, project: Option<(&str, &Path)>) -> (Value, String) {
    let entries: Vec<Value> = addins::discover(home, project)
        .iter()
        .map(|addin| entry(home, addin))
        .collect();
    let mut text = String::new();
    if entries.is_empty() {
        text.push_str("No add-ins here. Make one with: somtus addin new NAME\n");
    }
    for entry in &entries {
        let title = match entry["name"].as_str() {
            Some(name) => format!("{name} {}", entry["version"].as_str().unwrap_or_default()),
            None => "(unreadable)".to_owned(),
        };
        let trust = if entry["trusted"] == true {
            "trusted"
        } else {
            "not trusted"
        };
        text.push_str(&format!(
            "{}  {}  {}, {trust}\n  {}\n",
            entry["key"].as_str().unwrap_or_default(),
            title.trim_end(),
            entry["scope"].as_str().unwrap_or_default(),
            entry["path"].as_str().unwrap_or_default(),
        ));
        if let Some(permissions) =
            Some(joined(&entry["permissions"])).filter(|text| !text.is_empty())
        {
            text.push_str(&format!("  permissions: {permissions}\n"));
        }
        if let Some(workers) = Some(joined(&entry["workers"])).filter(|text| !text.is_empty()) {
            text.push_str(&format!("  workers: {workers}\n"));
        }
        for tool in entry["tools"].as_array().into_iter().flatten() {
            let agent = tool["agent_tool"]
                .as_str()
                .map(|name| format!(" ({name})"))
                .unwrap_or_default();
            text.push_str(&format!(
                "  tool: {}{agent}\n",
                tool["name"].as_str().unwrap_or_default()
            ));
        }
        for problem in entry["problems"].as_array().into_iter().flatten() {
            text.push_str(&format!(
                "  problem: {}\n",
                problem.as_str().unwrap_or_default()
            ));
        }
    }
    let value = json!({"v":1, "project_id": project.map(|(id, _)| id), "addins": entries});
    (value, text)
}

fn entry(home: &Path, addin: &Found) -> Value {
    let project_id = match &addin.scope {
        Scope::Project { project_id } => Some(project_id.as_str()),
        Scope::Personal => None,
    };
    let mut value = json!({
        "key": addin.key(), "id": null, "name": null, "version": null,
        "scope": scope_name(&addin.scope), "project_id": project_id, "path": addin.dir,
        "trusted": false, "permissions": [], "workers": [], "tools": [], "problems": [],
    });
    match &addin.manifest {
        Ok(manifest) => {
            let problems: Vec<String> = manifest
                .tools
                .iter()
                .filter_map(|tool| mcp::addin_tool_problem(&manifest.id, tool))
                .collect();
            let tools: Vec<Value> = manifest
                .tools
                .iter()
                .map(|tool| {
                    let offered = mcp::addin_tool_problem(&manifest.id, tool).is_none();
                    json!({
                        "name": tool.name, "description": tool.description,
                        "agent_tool": offered.then(|| mcp::addin_tool_name(&manifest.id, &tool.name)).flatten(),
                    })
                })
                .collect();
            let fields = json!({
                "id": manifest.id, "name": manifest.name, "version": manifest.version,
                "trusted": trusted(home, addin, manifest), "permissions": manifest.permissions,
                "workers": manifest.workers.iter().map(|worker| &worker.id).collect::<Vec<_>>(),
                "tools": tools, "problems": problems,
            });
            value
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
        }
        Err(error) => value["problems"] = json!([error]),
    }
    value
}

fn new(
    home: &Path,
    project: Option<(&str, &Path)>,
    mut args: Vec<String>,
) -> Result<(Value, String), Refusal> {
    let personal = take_flag(&mut args, "--personal");
    let id = take_option(&mut args, "--id").map_err(invalid)?;
    if args.is_empty() || args.iter().any(|arg| arg.starts_with("--")) {
        return Err(invalid(
            "Usage: somtus addin new NAME [--personal] [--id ID]",
        ));
    }
    let name = args.join(" ");
    let id = id.unwrap_or_else(|| addin_build::id_from_name(&name));
    let (parent, scope) = if personal {
        (addins::personal_dir(home), Scope::Personal)
    } else {
        let (project_id, root) = project.ok_or_else(|| {
            invalid("This folder is in no Somtus project: run it in a project's folder, or pass --personal")
        })?;
        (
            addins::project_dir(root),
            Scope::Project {
                project_id: project_id.to_owned(),
            },
        )
    };
    let dir: PathBuf = addin_build::scaffold(&parent, &id, &name).map_err(|message| {
        if let Some(reason) = message.strip_prefix("invalid: ") {
            invalid(reason)
        } else if message.ends_with("exists already") {
            ("already_exists", message)
        } else {
            internal(message)
        }
    })?;
    let key = format!("{}/{id}", scope.key());
    let name = name.trim();
    let text = format!(
        "Made {name} ({key}) in {}\nEdit App.tsx and addin.json there; it runs once you trust it: somtus addin trust {key}\n",
        dir.display()
    );
    let value = json!({"v":1, "key": key, "id": id, "name": name, "scope": scope_name(&scope), "path": dir});
    Ok((value, text))
}

/// What trusting `manifest` grants: as JSON fields, and as text.
fn grant(manifest: &Manifest) -> (Value, String) {
    let mut text = String::new();
    if manifest.permissions.is_empty() {
        text.push_str("  no permissions: only its own views, storage and settings\n");
    }
    let permissions: Vec<Value> = manifest
        .permissions
        .iter()
        .map(|permission| {
            let name = serde_json::to_value(permission).unwrap_or(Value::Null);
            let origins = match permission {
                addins::Permission::Network => format!(": {}", manifest.network.join(", ")),
                _ => String::new(),
            };
            text.push_str(&format!(
                "  {} ({}){origins}\n",
                permission.describe(),
                name.as_str().unwrap_or_default()
            ));
            json!({"permission": name, "description": permission.describe()})
        })
        .collect();
    if !manifest.workers.is_empty() {
        text.push_str("It runs these programs of its own, with your rights:\n");
    }
    for worker in &manifest.workers {
        let about = if worker.description.is_empty() {
            String::new()
        } else {
            format!(" ({})", worker.description)
        };
        text.push_str(&format!(
            "  {}: {}{about}\n",
            worker.id,
            worker.command.join(" ")
        ));
    }
    if !manifest.tools.is_empty() {
        text.push_str("Agents get these tools of it (somtus mcp):\n");
    }
    let tools: Vec<Value> = manifest
        .tools
        .iter()
        .map(|tool| {
            let agent_tool = mcp::addin_tool_name(&manifest.id, &tool.name);
            text.push_str(&format!(
                "  {}: {} (worker {}, method {})\n",
                agent_tool.as_deref().unwrap_or(&tool.name),
                tool.description,
                tool.worker,
                tool.method
            ));
            json!({"name": tool.name, "agent_tool": agent_tool, "description": tool.description, "worker": tool.worker, "method": tool.method})
        })
        .collect();
    let value = json!({"permissions": permissions, "network": manifest.network, "workers": manifest.workers, "tools": tools});
    (value, text)
}

fn trust(
    home: &Path,
    project: Option<(&str, &Path)>,
    key: &str,
) -> Result<(Value, String), Refusal> {
    let addin = find(home, project, key)?;
    let manifest = addin
        .manifest
        .as_ref()
        .map_err(|error| unreadable(&addin, error))?;
    let key = addin.key();
    let (granted, listed) = grant(manifest);
    addins::trust(home, &key, &addins::grant_digest(&addin.dir, manifest)).map_err(internal)?;
    let text = format!("{} ({key}) may:\n{listed}Trusted {key}.\n", manifest.name);
    let mut value =
        json!({"v":1, "key": key, "id": manifest.id, "name": manifest.name, "trusted": true});
    value
        .as_object_mut()
        .unwrap()
        .extend(granted.as_object().unwrap().clone());
    Ok((value, text))
}

fn revoke(
    home: &Path,
    project: Option<(&str, &Path)>,
    key: &str,
) -> Result<(Value, String), Refusal> {
    let addin = match find(home, project, key) {
        Ok(addin) => Some(addin),
        // An add-in that is gone can still lose its grant, by its whole key.
        Err(_) if key.contains('/') => None,
        Err(refusal) => return Err(refusal),
    };
    let key = addin.as_ref().map_or_else(|| key.to_owned(), Found::key);
    addins::revoke(home, &key).map_err(internal)?;
    let mut text = String::new();
    if let Some((addin, manifest)) = addin
        .as_ref()
        .and_then(|addin| Some((addin, addin.manifest.as_ref().ok()?)))
    {
        let (_, listed) = grant(manifest);
        text.push_str(&format!(
            "{} ({}) no longer may:\n{listed}",
            manifest.name,
            addin.key()
        ));
    }
    text.push_str(&format!(
        "Revoked {key}: nothing of it runs until it is trusted again.\n"
    ));
    Ok((json!({"v":1, "key": key, "trusted": false}), text))
}

fn tool(
    home: &Path,
    project: Option<(&str, &Path)>,
    args: Vec<String>,
) -> Result<(Value, String), Refusal> {
    let (key, name, input) = match args.as_slice() {
        [key, name] => (key, name, None),
        [key, name, input] => (key, name, Some(input)),
        _ => return Err(invalid("Usage: somtus addin tool KEY TOOL [JSON]")),
    };
    let arguments: Value = match input {
        Some(text) => serde_json::from_str(text)
            .map_err(|error| invalid(format!("The tool's arguments are not JSON: {error}")))?,
        None => json!({}),
    };
    if !arguments.is_object() {
        return Err(invalid("The tool's arguments must be one JSON object"));
    }
    let addin = find(home, project, key)?;
    let manifest = addin
        .manifest
        .as_ref()
        .map_err(|error| unreadable(&addin, error))?;
    let Some(tool) = manifest.tools.iter().find(|tool| tool.name == *name) else {
        let names: Vec<&str> = manifest
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        return Err((
            "not_found",
            format!(
                "{} has no tool {name} (it has: {})",
                manifest.name,
                if names.is_empty() {
                    "none".to_owned()
                } else {
                    names.join(", ")
                }
            ),
        ));
    };
    if !trusted(home, &addin, manifest) {
        return Err((
            "not_trusted",
            format!(
                "{} is not trusted with what it asks for now; run somtus addin trust {}",
                manifest.name,
                addin.key()
            ),
        ));
    }
    let result = mcp::run_addin_tool(home, &addin, manifest, tool, arguments);
    mcp::stop_addin_workers();
    let value = result.map_err(|error| ("tool_failed", error))?;
    let text =
        serde_json::to_string_pretty(&value).map_err(|error| internal(error.to_string()))? + "\n";
    Ok((json!({"v":1, "result": value}), text))
}
