//! `riwork native invoke MODULE.METHOD --json`: one call of a `riwork/native` method for a
//! paired device, made by the remote connector (`native.invoke` in `docs/remote-protocol.md`).
//!
//! The arguments arrive on standard input as one JSON object; the answer is one JSON line,
//! `{"v":1,"value":…}` or `{"v":1,"error":{"code","message"}}`. Three layers decide what runs:
//!
//! 1. [`METHODS`], a static table. Nothing else is callable; a method that holds state between
//!    calls (subscriptions, chat handles, staged memos, decision reviews), creates a project at a
//!    path, takes a working directory, or changes billing, accounts, API keys, windows or the
//!    status bar is not in it.
//! 2. A typed argument struct per method that refuses unknown fields.
//! 3. An argument policy per method: ids are canonical UUIDs and strings are bounded; no method
//!    takes a filesystem path the device chose. `Files.*` and `Sessions.openEditor` take a
//!    project (and worktree) id and a path relative to it; the Mac resolves the root from its
//!    store and walks it by descriptor without following links ([`crate::files::confined_path`]).
//!    Policy changes may only narrow what agents may do, and schedules may only be paused or
//!    deleted.
//!
//! The call runs on a detached process service ([`crate::service::detached_process`]): the
//! desktop app's backends and checks, without its home watcher or session sampler. Writes take
//! the same cross-process file locks as the desktop app's (the store's `state.lock`, the
//! settings' and the schedules' locks): each is a read-modify-write under an exclusive lock, so
//! a write here and one in the running app never lose each other. The app's watcher sees the
//! change and republishes it.
//!
//! The value is what JavaScript sees from the same method on the Mac: serde output, with every
//! `#[prop(skip_none)]` field also skipped by serde when it is `None`.
use std::{io::Read, path::PathBuf, sync::Arc};

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
    dto::{CreateSubshellRequest, OpenEditorRequest, StoreState},
    files,
    schedules::{EngineSchedules, ScheduleKeyDto, ScheduleScopeDto},
    service::{self, ProcessService, blocking},
    settings::{DelegationPatch, SettingsPatch},
};

/// One callable method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Method {
    /// `Module.method`, as JavaScript names it.
    pub name: &'static str,
    /// Changes something: the connector runs it in the ordered lane.
    pub write: bool,
    /// The most bytes of JSON its value may have; a larger one is `response_too_large`.
    pub max_bytes: usize,
}

const SMALL: usize = 128 * 1024;
/// What the connector may carry compressed (`link::MAX_INFLATED`); whether it then fits one
/// frame is the connector's check.
const LARGE: usize = 2 * 1024 * 1024;
/// An image or rendered PDF page: base64 PNG hardly compresses, so more than this cannot fit
/// one 128 KiB frame.
const MEDIA: usize = 96 * 1024;

const fn read(name: &'static str, max_bytes: usize) -> Method {
    Method {
        name,
        write: false,
        max_bytes,
    }
}
const fn write(name: &'static str) -> Method {
    Method {
        name,
        write: true,
        max_bytes: SMALL,
    }
}

/// Allowlist v1. The connector (`remote/src/rpc/native.rs`) and the phone
/// (`ios/Core/RelayClient.swift`, `NativeInvokeRequests`) keep copies; the connector offers
/// only the methods both it and this table have.
pub const METHODS: &[Method] = &[
    read("Store.snapshot", LARGE),
    read("Store.search", LARGE),
    write("Store.renameProject"),
    write("Store.updateProjectMetadata"),
    write("Store.setProjectMemo"),
    write("Store.setProjectNotifications"),
    write("Store.setProjectOrchestratorWakes"),
    write("Store.setProjectOrchestrator"),
    write("Store.setProjectDelegationPolicy"),
    write("Store.createFolder"),
    write("Store.renameFolder"),
    write("Store.moveFolder"),
    write("Store.removeFolder"),
    write("Store.moveProjectToFolder"),
    write("Store.addTask"),
    write("Store.assignTasks"),
    write("Store.unassignTasks"),
    write("Store.setTaskStatus"),
    write("Store.refreshWorktrees"),
    write("Store.syncWorktrees"),
    read("Sessions.list", LARGE),
    write("Sessions.createSubshell"),
    write("Sessions.openEditor"),
    read("Tabs.snapshot", LARGE),
    read("Settings.get", SMALL),
    write("Settings.set"),
    read("Files.listDirectory", LARGE),
    read("Files.preview", LARGE),
    read("Files.readText", LARGE),
    read("Schedules.list", LARGE),
    write("Schedules.pause"),
    write("Schedules.delete"),
    read("Orchestrators.plan", SMALL),
];

/// The topics `riwork watch` reports on (`crate::state_watch`).
pub use crate::state_watch::TOPICS;

pub fn method(name: &str) -> Option<&'static Method> {
    METHODS.iter().find(|method| method.name == name)
}

/// The most standard input a call reads: a memo or a task's details with JSON escaping.
const MAX_ARGS: usize = 256 * 1024;
/// A listing page when the device names none, and the most it may ask for.
const PAGE_DEFAULT: usize = 500;
const PAGE_MAX: usize = 1000;
const NAME_MAX: usize = 200;
const QUERY_MAX: usize = 512;
const TITLE_MAX: usize = 500;
const TEXT_MAX: usize = 32 * 1024;
const IDS_MAX: usize = 500;

/// What the device may send for `riwork native invoke` and `riwork watch`, and their answers.
/// `args` are everything after the binary's name. `None` when they are not one of these
/// commands, so the ordinary CLI and the app see them.
pub fn cli(args: &[String]) -> Option<i32> {
    let json = args.iter().any(|arg| arg == "--json");
    let words = args
        .iter()
        .filter(|arg| *arg != "--json")
        .map(String::as_str)
        .collect::<Vec<_>>();
    let answer = match words.as_slice() {
        ["native", "invoke", target, rest @ ..] => {
            if !rest.is_empty() || !json {
                return Some(usage());
            }
            match stdin(MAX_ARGS) {
                Ok(input) => invoke(target, &input),
                Err(error) => failure(&error),
            }
        }
        ["native", "invoke", ..] => return Some(usage()),
        // Only with `--json`: `riwork watch` alone may name a folder to open.
        ["watch", rest @ ..] if json => {
            if !rest.is_empty() {
                return Some(usage());
            }
            match stdin(64 * 1024) {
                Ok(input) => crate::state_watch::run(&input),
                Err(error) => failure(&error),
            }
        }
        _ => return None,
    };
    println!("{answer}");
    Some(0)
}

fn usage() -> i32 {
    eprintln!(
        "riwork: Usage: riwork native invoke MODULE.METHOD --json < ARGS, or riwork watch --json < REQUEST"
    );
    2
}

/// What `riwork capabilities --json` adds for these commands: `native` (`{"v":1,"methods":
/// [...]}`) and `watch` (`{"v":1,"topics":[...]}`). The connector announces what it and this
/// binary both have.
pub fn capabilities() -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    out.insert(
        "native".into(),
        json!({"v": 1, "methods": METHODS.iter().map(|m| m.name).collect::<Vec<_>>()}),
    );
    out.insert("watch".into(), json!({"v": 1, "topics": TOPICS}));
    out
}

fn stdin(cap: usize) -> Result<Vec<u8>, String> {
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .take(cap as u64 + 1)
        .read_to_end(&mut input)
        .map_err(|error| format!("io: cannot read the arguments: {error}"))?;
    if input.len() > cap {
        return Err(format!(
            "invalid: the arguments are longer than {cap} bytes"
        ));
    }
    Ok(input)
}

/// The answer to one call: the JSON line `riwork native invoke` prints.
pub fn invoke(target: &str, input: &[u8]) -> Value {
    let Some(method) = method(target) else {
        return failure("invalid: unsupported native method");
    };
    let args: Value = if input.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        match serde_json::from_slice(input) {
            Ok(args @ Value::Object(_)) => args,
            Ok(_) => return failure("invalid: the arguments must be a JSON object"),
            Err(error) => return failure(&format!("invalid: {error}")),
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return failure(&format!("io: {error}")),
    };
    match runtime.block_on(call(method, args)) {
        Ok(answer) => {
            let size = serde_json::to_vec(&answer.value).map_or(usize::MAX, |bytes| bytes.len());
            if size > method.max_bytes {
                return failure("limit: Too large to show on the phone");
            }
            let mut out = json!({"v": 1, "value": answer.value});
            if let Some(page) = answer.page {
                out["page"] = page;
            }
            out
        }
        Err(error) => failure(&missing_is_not_found(error)),
    }
}

/// A backend's answer that an id names nothing on the Mac, coded `not_found`. The store says
/// `No project matches '…'` (or worktree, task, project folder, repository), the sessions
/// registry `unknown shell …` and the sessions backend `unknown project`; they reach here
/// coded `io` or `refused`, which would read as a failure of the desktop or of its policy.
fn missing_is_not_found(error: String) -> String {
    let Some((code, message)) = error.split_once(": ") else {
        return error;
    };
    let missing = match code {
        "io" => {
            (message.starts_with("No ") && message.contains(" matches '"))
                || message.starts_with("unknown shell ")
        }
        "refused" => message == "unknown project",
        _ => false,
    };
    if missing {
        format!("not_found: {message}")
    } else {
        error
    }
}

/// `{"v":1,"error":…}` for a bridge error (`prefix: message`), coded for the wire.
pub(crate) fn failure(error: &str) -> Value {
    let (prefix, message) = error
        .split_once(": ")
        .filter(|(prefix, _)| !prefix.contains(' '))
        .unwrap_or(("", error));
    let code = match prefix {
        "invalid" => "invalid_request",
        "not_found" => "not_found",
        "conflict" => "conflict",
        "refused" => "refused",
        "limit" => "response_too_large",
        _ => "cli_error",
    };
    let message = if code == "cli_error" { error } else { message };
    json!({"v": 1, "error": {"code": code, "message": message}})
}

struct Answer {
    value: Value,
    page: Option<Value>,
}

fn value(value: impl serde::Serialize) -> Result<Answer, String> {
    Ok(Answer {
        value: serde_json::to_value(value).map_err(|error| format!("io: {error}"))?,
        page: None,
    })
}

fn args<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|error| format!("invalid: {error}"))
}

fn uuid(id: &str) -> Result<(), String> {
    service::validate_uuid(id)
}

fn optional_uuid(id: Option<&str>) -> Result<(), String> {
    id.map(uuid).transpose().map(|_| ())
}

fn bounded(text: &str, max: usize, what: &str) -> Result<(), String> {
    if text.len() > max {
        Err(format!("invalid: {what} is longer than {max} bytes"))
    } else {
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Query {
    query: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Rename {
    selector: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Metadata {
    project: String,
    name: String,
    #[serde(default)]
    folder: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Memo {
    project: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Switch {
    selector: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Orchestrator {
    project: String,
    choice: Value,
}

/// A delegation change that can only narrow: stop fanning work out. Raising the number of
/// worker shells, choosing routes and clearing an override (which may widen) are made on the
/// Mac.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NarrowDelegation {
    spread_work: bool,
}

impl NarrowDelegation {
    fn patch(self) -> Result<DelegationPatch, String> {
        if self.spread_work {
            return Err(widening());
        }
        Ok(DelegationPatch {
            spread_work: Some(Some(false)),
            ..DelegationPatch::default()
        })
    }
}

fn widening() -> String {
    "refused: a change that widens what agents may do is made on the Mac".into()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Delegation {
    project: String,
    patch: NarrowDelegation,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NewFolder {
    name: String,
    #[serde(default)]
    parent: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MoveFolder {
    selector: String,
    #[serde(default)]
    parent: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Selector {
    selector: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MoveProject {
    selector: String,
    #[serde(default)]
    folder: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NewTask {
    project: String,
    title: String,
    details: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Assign {
    worktree: String,
    tasks: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Unassign {
    tasks: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskStatus {
    selector: String,
    status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Subshell {
    request: CreateSubshellRequest,
}

/// `OpenEditorRequest` with a path relative to the project or worktree.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Editor {
    request: OpenEditorRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectTabs {
    project_id: String,
}

/// The policy part of `SettingsPatch` that only narrows: agent terminals no longer run
/// unrestricted, and work is no longer fanned out.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NarrowSettings {
    #[serde(default)]
    agent_terminals_unrestricted: Option<bool>,
    #[serde(default)]
    delegation_patch: Option<NarrowDelegation>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetSettings {
    patch: NarrowSettings,
}

impl NarrowSettings {
    fn patch(self) -> Result<SettingsPatch, String> {
        if self.agent_terminals_unrestricted == Some(true) {
            return Err(widening());
        }
        if self.agent_terminals_unrestricted.is_none() && self.delegation_patch.is_none() {
            return Err("invalid: the patch changes nothing".into());
        }
        Ok(SettingsPatch {
            agent_terminals_unrestricted: self.agent_terminals_unrestricted,
            delegation_patch: self
                .delegation_patch
                .map(NarrowDelegation::patch)
                .transpose()?,
            ..SettingsPatch::default()
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Listing {
    project_id: String,
    #[serde(default)]
    worktree_id: Option<String>,
    path: String,
    #[serde(default)]
    show_hidden: bool,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Preview {
    project_id: String,
    #[serde(default)]
    worktree_id: Option<String>,
    path: String,
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    pdf_requested: Option<bool>,
    #[serde(default)]
    page: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadText {
    project_id: String,
    #[serde(default)]
    worktree_id: Option<String>,
    path: String,
    identity: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScheduleList {
    #[serde(default)]
    scope: Option<ScheduleScopeDto>,
}

/// Pausing only: resuming makes a schedule run unattended again, which is done on the Mac.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SchedulePause {
    key: ScheduleKeyDto,
    paused: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScheduleKey {
    key: ScheduleKeyDto,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    #[serde(default)]
    project_id: Option<String>,
}

/// The root of `project_id` (or of its worktree `worktree_id`) in the Mac's own store: the
/// only roots another device's paths are taken relative to.
pub fn project_root(
    state: &StoreState,
    project_id: &str,
    worktree_id: Option<&str>,
) -> Result<PathBuf, String> {
    uuid(project_id)?;
    optional_uuid(worktree_id)?;
    let project = state
        .projects
        .iter()
        .find(|project| project.id == project_id)
        .ok_or("not_found: no such project on the Mac")?;
    let root = match worktree_id {
        None => PathBuf::from(&project.root),
        Some(id) => PathBuf::from(
            &state
                .worktrees
                .iter()
                .find(|tree| tree.id == id && tree.project_id == project_id)
                .ok_or("refused: the worktree does not belong to the project")?
                .path,
        ),
    };
    if !root.is_absolute() {
        return Err("refused: the project root is not an absolute path".into());
    }
    Ok(root)
}

async fn root_of(
    service: &Arc<ProcessService>,
    project_id: &str,
    worktree_id: Option<&str>,
) -> Result<PathBuf, String> {
    // Checked before the store is read: a malformed id is refused as such.
    uuid(project_id)?;
    optional_uuid(worktree_id)?;
    let state = service.store_snapshot().await?;
    project_root(&state, project_id, worktree_id)
}

async fn call(method: &Method, raw: Value) -> Result<Answer, String> {
    let service = blocking(service::detached_process).await?;
    let schedules = || EngineSchedules::new(service.home().to_owned());
    match method.name {
        "Store.snapshot" => {
            let NoArgs {} = args(raw)?;
            value(service.store_snapshot().await?)
        }
        "Store.search" => {
            let Query { query } = args(raw)?;
            bounded(&query, QUERY_MAX, "query")?;
            value(service.store_search(query).await?)
        }
        "Store.renameProject" => {
            let Rename { selector, name } = args(raw)?;
            bounded(&name, NAME_MAX, "name")?;
            value(service.rename_project(selector, name).await?)
        }
        "Store.updateProjectMetadata" => {
            let Metadata {
                project,
                name,
                folder,
            } = args(raw)?;
            bounded(&name, NAME_MAX, "name")?;
            value(
                service
                    .update_project_metadata(project, name, folder)
                    .await?,
            )
        }
        "Store.setProjectMemo" => {
            let Memo { project, text } = args(raw)?;
            bounded(&text, TEXT_MAX, "text")?;
            // No staged revision: drafts are process-local, and the last save wins.
            value(service.set_project_memo(project, text, None).await?)
        }
        "Store.setProjectNotifications" => {
            let Switch { selector, enabled } = args(raw)?;
            value(service.set_project_notifications(selector, enabled).await?)
        }
        "Store.setProjectOrchestratorWakes" => {
            let Switch { selector, enabled } = args(raw)?;
            value(
                service
                    .set_project_orchestrator_wakes(selector, enabled)
                    .await?,
            )
        }
        "Store.setProjectOrchestrator" => {
            let Orchestrator { project, choice } = args(raw)?;
            bounded(&choice.to_string(), 1024, "choice")?;
            value(service.set_project_orchestrator(project, choice).await?)
        }
        "Store.setProjectDelegationPolicy" => {
            let Delegation { project, patch } = args(raw)?;
            value(
                service
                    .set_project_delegation_policy(project, patch.patch()?)
                    .await?,
            )
        }
        "Store.createFolder" => {
            let NewFolder { name, parent } = args(raw)?;
            bounded(&name, NAME_MAX, "name")?;
            value(service.create_folder(name, parent).await?)
        }
        "Store.renameFolder" => {
            let Rename { selector, name } = args(raw)?;
            bounded(&name, NAME_MAX, "name")?;
            value(service.rename_folder(selector, name).await?)
        }
        "Store.moveFolder" => {
            let MoveFolder { selector, parent } = args(raw)?;
            value(service.move_folder(selector, parent).await?)
        }
        "Store.removeFolder" => {
            let Selector { selector } = args(raw)?;
            value(service.remove_folder(selector).await?)
        }
        "Store.moveProjectToFolder" => {
            let MoveProject { selector, folder } = args(raw)?;
            value(service.move_project_to_folder(selector, folder).await?)
        }
        "Store.addTask" => {
            let NewTask {
                project,
                title,
                details,
            } = args(raw)?;
            bounded(&title, TITLE_MAX, "title")?;
            bounded(&details, TEXT_MAX, "details")?;
            value(service.add_task(project, title, details).await?)
        }
        "Store.assignTasks" => {
            let Assign { worktree, tasks } = args(raw)?;
            if tasks.len() > IDS_MAX {
                return Err(format!("invalid: at most {IDS_MAX} tasks"));
            }
            value(service.assign_tasks(worktree, tasks).await?)
        }
        "Store.unassignTasks" => {
            let Unassign { tasks } = args(raw)?;
            if tasks.len() > IDS_MAX {
                return Err(format!("invalid: at most {IDS_MAX} tasks"));
            }
            value(service.unassign_tasks(tasks).await?)
        }
        "Store.setTaskStatus" => {
            let TaskStatus { selector, status } = args(raw)?;
            value(service.set_task_status(selector, status).await?)
        }
        "Store.refreshWorktrees" => {
            let Selector { selector } = args(raw)?;
            value(service.refresh_worktrees(selector).await?)
        }
        "Store.syncWorktrees" => {
            let Selector { selector } = args(raw)?;
            value(service.sync_worktrees(selector).await?)
        }
        "Sessions.list" => {
            let NoArgs {} = args(raw)?;
            value(service.sessions_list().await?)
        }
        "Sessions.createSubshell" => {
            let Subshell { request } = args(raw)?;
            value(service.create_subshell(request).await?)
        }
        "Sessions.openEditor" => {
            let Editor { mut request } = args(raw)?;
            let root = root_of(
                &service,
                &request.project_id,
                request.worktree_id.as_deref(),
            )
            .await?;
            // An existing regular file inside the project: the backend walks it again
            // without following links before Vim opens it.
            request.path = files::confined_path(&root, &request.path, false)?
                .to_str()
                .ok_or("invalid: the path is not UTF-8")?
                .to_owned();
            if let Some(identity) = &request.identity {
                bounded(identity, 512, "identity")?;
            }
            value(service.open_editor(request).await?)
        }
        "Tabs.snapshot" => {
            let ProjectTabs { project_id } = args(raw)?;
            value(service.tabs_snapshot(project_id).await?)
        }
        "Settings.get" => {
            let NoArgs {} = args(raw)?;
            let preferences = service.preferences().clone();
            value(blocking(move || Ok(preferences.get())).await?)
        }
        "Settings.set" => {
            let SetSettings { patch } = args(raw)?;
            let patch = patch.patch()?;
            let preferences = service.preferences().clone();
            value(blocking(move || preferences.set(patch)).await?)
        }
        "Files.listDirectory" => {
            let Listing {
                project_id,
                worktree_id,
                path,
                show_hidden,
                offset,
                limit,
            } = args(raw)?;
            let limit = limit.unwrap_or(PAGE_DEFAULT);
            if limit == 0 || limit > PAGE_MAX {
                return Err(format!("invalid: limit must be 1 to {PAGE_MAX}"));
            }
            let offset = offset.unwrap_or(0);
            let root = root_of(&service, &project_id, worktree_id.as_deref()).await?;
            let mut listing =
                blocking(move || files::list_relative(&root, &path, show_hidden)).await?;
            let total = listing.entries.len();
            listing.entries = listing
                .entries
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect();
            let mut answer = value(listing)?;
            answer.page = Some(json!({"offset": offset, "limit": limit, "total": total}));
            Ok(answer)
        }
        "Files.preview" => {
            let Preview {
                project_id,
                worktree_id,
                path,
                identity,
                pdf_requested,
                page,
            } = args(raw)?;
            let root = root_of(&service, &project_id, worktree_id.as_deref()).await?;
            let preview = blocking(move || {
                files::preview_relative(
                    &root,
                    &path,
                    identity.as_deref(),
                    pdf_requested.unwrap_or(false),
                    page.unwrap_or(1),
                )
            })
            .await?;
            // A picture's base64 hardly compresses: past this it cannot reach the phone.
            if preview
                .data_url
                .as_ref()
                .is_some_and(|url| url.len() > MEDIA)
            {
                return Err("limit: Too large to show on the phone".into());
            }
            value(preview)
        }
        "Files.readText" => {
            let ReadText {
                project_id,
                worktree_id,
                path,
                identity,
            } = args(raw)?;
            let root = root_of(&service, &project_id, worktree_id.as_deref()).await?;
            value(blocking(move || files::read_text_relative(&root, &path, &identity)).await?)
        }
        "Schedules.list" => {
            let ScheduleList { scope } = args(raw)?;
            let schedules = schedules();
            value(
                blocking(move || {
                    schedules
                        .list(scope.as_ref())
                        .map_err(crate::schedules::bridge_error)
                })
                .await?,
            )
        }
        "Schedules.pause" => {
            let SchedulePause { key, paused } = args(raw)?;
            if !paused {
                return Err(
                    "refused: a paused schedule is resumed on the Mac, where it runs unattended"
                        .into(),
                );
            }
            let schedules = schedules();
            value(
                blocking(move || {
                    schedules
                        .pause(&key, true)
                        .map_err(crate::schedules::bridge_error)
                })
                .await?,
            )
        }
        "Schedules.delete" => {
            let ScheduleKey { key } = args(raw)?;
            let schedules = schedules();
            value(
                blocking(move || {
                    schedules
                        .delete(&key)
                        .map_err(crate::schedules::bridge_error)
                })
                .await?,
            )
        }
        "Orchestrators.plan" => {
            let Plan { project_id } = args(raw)?;
            value(crate::orchestrators::Orchestrators::plan(project_id).await?)
        }
        _ => Err("invalid: unsupported native method".into()),
    }
}
