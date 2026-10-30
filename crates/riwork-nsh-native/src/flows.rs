//! Flows in the desktop: a project's statecharts (`.ontack/flows`), the chats they lead, and
//! their diagrams. The same engine as `riwork flow`: the UI starts, approves, turns down and
//! stops runs; the agent moves on with the CLI. A view's snapshot is read again when a file
//! watcher sees the runs folder or a watched project's flows change (and every 15 s, for what
//! a watcher can miss), and posted only when it changed.
use std::{
    collections::{BTreeSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use notify::{RecursiveMode, Watcher};

use nsh_core::prelude::*;
use riwork_core::flows::{self as model, Flow};
use riwork_engine::{
    chat::{client::Client, model::ChatCommand},
    flows::{self as runs, ChatRef, Run, Status},
    store::Store,
};
use serde::{Deserialize, Serialize};

use crate::{Subscription, hub::Hub, service};

const FALLBACK: Duration = Duration::from_secs(15);
const FINISHED_SHOWN: usize = 12;

/// One watcher for the process: a change bumps the generation every view waits on.
struct Watch {
    changes: Hub<u64>,
    generation: AtomicU64,
    watcher: Mutex<notify::RecommendedWatcher>,
    watched: Mutex<BTreeSet<PathBuf>>,
}

impl Watch {
    fn bump(&self) {
        self.changes.publish(self.generation.fetch_add(1, Ordering::AcqRel) + 1);
    }

    /// Watches a project's flows folder, or, until it exists, the nearest folder that will hold
    /// it: a new `.ontack/flows` is seen when it is made.
    fn follow(&self, root: &Path) {
        let folder = model::project_dir(root);
        let target = [folder.clone(), folder.parent().map(Path::to_path_buf).unwrap_or_default(), root.to_path_buf()]
            .into_iter()
            .find(|path| path.is_dir());
        if let Some(target) = target {
            let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
            if watched.insert(target.clone()) {
                let _ = self.watcher.lock().unwrap_or_else(PoisonError::into_inner).watch(&target, RecursiveMode::NonRecursive);
            }
        }
    }
}

fn watch() -> Result<Arc<Watch>, String> {
    static WATCH: OnceLock<Arc<Watch>> = OnceLock::new();
    static START: Mutex<()> = Mutex::new(());
    let _start = START.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(watch) = WATCH.get() {
        return Ok(watch.clone());
    }
    let runs_dir = runs::runs_dir(&service::process_home()?);
    std::fs::create_dir_all(&runs_dir).map_err(service::io_error)?;
    let slot: Arc<OnceLock<std::sync::Weak<Watch>>> = Arc::default();
    let seen = slot.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if matches!(event, Ok(ref event) if matches!(event.kind, notify::EventKind::Access(_))) {
            return;
        }
        if let Some(watch) = seen.get().and_then(std::sync::Weak::upgrade) {
            watch.bump();
        }
    })
    .map_err(|error| format!("Cannot watch flows: {error}"))?;
    watcher.watch(&runs_dir, RecursiveMode::NonRecursive).map_err(|error| format!("Cannot watch flows: {error}"))?;
    let watch = Arc::new(Watch {
        changes: Hub::new(0),
        generation: AtomicU64::new(0),
        watcher: Mutex::new(watcher),
        watched: Mutex::new(BTreeSet::new()),
    });
    let _ = slot.set(Arc::downgrade(&watch));
    let ticker = Arc::downgrade(&watch);
    std::thread::Builder::new()
        .name("riwork-flows-watch".into())
        .spawn(move || {
            while let Some(watch) = ticker.upgrade() {
                drop(watch);
                std::thread::sleep(FALLBACK);
                if let Some(watch) = ticker.upgrade() {
                    watch.bump();
                }
            }
        })
        .map_err(service::io_error)?;
    let _ = WATCH.set(watch.clone());
    Ok(watch)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct FlowDto {
    pub id: String,
    pub path: String,
    pub name: String,
    pub description: String,
    /// Why the file is not a valid flow.
    #[prop(skip_none)]
    pub error: Option<String>,
    /// The flow's diagram source, with no state marked.
    pub mermaid: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct FlowEventDto {
    pub event: String,
    pub target: String,
    pub description: String,
    /// `approval`, `check` or `none`.
    pub gate: String,
    #[prop(skip_none)]
    pub command: Option<String>,
    pub evidence: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct FlowStepDto {
    pub at: u64,
    pub kind: String,
    pub from: String,
    pub event: String,
    pub to: String,
    pub evidence: String,
    pub detail: String,
    pub by: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct FlowRunDto {
    pub id: String,
    pub flow_id: String,
    pub flow_name: String,
    pub project_id: String,
    pub chat_id: String,
    pub chat_title: String,
    pub state: String,
    /// Where the run is, as "Outer › Inner".
    pub location: String,
    /// `active`, `waiting`, `completed` or `stopped`.
    pub status: String,
    #[prop(skip_none)]
    pub pending_event: Option<String>,
    #[prop(skip_none)]
    pub pending_location: Option<String>,
    #[prop(skip_none)]
    pub pending_evidence: Option<String>,
    /// How the agent may move on from here.
    pub events: Vec<FlowEventDto>,
    pub history: Vec<FlowStepDto>,
    /// The diagram source with the run's state marked; empty when the flow cannot be read.
    pub mermaid: String,
    /// Why the run's flow cannot be read now (removed, invalid, or without the run's state).
    #[prop(skip_none)]
    pub error: Option<String>,
    pub started_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct FlowsSnapshot {
    /// The project's flows folder.
    pub folder: String,
    pub flows: Vec<FlowDto>,
    /// The project's live runs, then its latest finished ones.
    pub runs: Vec<FlowRunDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct FlowChatDto {
    /// The chat's live run, if a flow leads it.
    #[prop(skip_none)]
    pub run: Option<FlowRunDto>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct FlowDiagramDto {
    pub data_url: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsObject)]
#[serde(rename_all = "camelCase")]
pub struct FlowCreatedDto {
    pub id: String,
    pub path: String,
}

fn project_root(project: &str) -> Result<PathBuf, String> {
    let home = service::process_home()?;
    Ok(Store::open(home)?.snapshot()?.project(project)?.root.clone())
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Active => "active",
        Status::Waiting => "waiting",
        Status::Completed => "completed",
        Status::Stopped => "stopped",
    }
}

fn events(flow: &Flow, state: &str) -> Vec<FlowEventDto> {
    flow.events(state)
        .into_iter()
        .map(|(event, transition)| {
            let (gate, command) = match &transition.check {
                Some(check) if check.approval => ("approval", None),
                Some(check) => ("check", check.command.clone()),
                None => ("none", None),
            };
            FlowEventDto {
                event,
                target: flow.location(&flow.enter(&transition.target).unwrap_or(transition.target.clone())),
                description: transition.description.clone(),
                gate: gate.into(),
                command,
                evidence: transition.evidence,
            }
        })
        .collect()
}

fn run_dto(run: &Run, flow: Result<&Flow, &String>) -> FlowRunDto {
    let flow = match flow {
        Ok(flow) if flow.state(&run.state).is_some() => Ok(flow),
        Ok(_) => Err(format!("The flow no longer has the state {:?}.", run.state)),
        Err(error) => Err(error.clone()),
    };
    let (location, events, mermaid, error) = match &flow {
        Ok(flow) => (
            flow.location(&run.state),
            if run.status == Status::Active { events(flow, &run.state) } else { Vec::new() },
            flow.mermaid(Some(&run.state)),
            None,
        ),
        Err(error) => (run.state.clone(), Vec::new(), String::new(), Some(error.clone())),
    };
    let pending = run.pending.as_ref();
    FlowRunDto {
        id: run.id.clone(),
        flow_id: run.flow_id.clone(),
        flow_name: run.flow_name.clone(),
        project_id: run.project_id.clone(),
        chat_id: run.chat_id.clone(),
        chat_title: run.chat_title.clone(),
        state: run.state.clone(),
        location,
        status: status_name(run.status).into(),
        pending_event: pending.map(|pending| pending.event.clone()),
        pending_location: pending.map(|pending| flow.as_ref().map_or(pending.target.clone(), |flow| flow.location(&flow.enter(&pending.target).unwrap_or(pending.target.clone())))),
        pending_evidence: pending.map(|pending| pending.evidence.clone()).filter(|evidence| !evidence.is_empty()),
        events,
        history: run
            .history
            .iter()
            .map(|step| FlowStepDto {
                at: step.at, kind: step.kind.clone(), from: step.from.clone(), event: step.event.clone(),
                to: step.to.clone(), evidence: step.evidence.clone(), detail: step.detail.clone(), by: step.by.clone(),
            })
            .collect(),
        mermaid,
        error,
        started_at: run.started_at,
        updated_at: run.updated_at,
    }
}

fn read_run(run: &Run) -> FlowRunDto {
    let flow = model::find(&run.root, &run.flow_id);
    run_dto(run, flow.as_ref())
}

fn snapshot(project: &str) -> Result<FlowsSnapshot, String> {
    let home = service::process_home()?;
    let root = project_root(project)?;
    let found = model::discover(&root);
    let flows = found
        .iter()
        .map(|found| FlowDto {
            id: found.id.clone(),
            path: found.path.to_string_lossy().into_owned(),
            name: found.flow.as_ref().map_or(found.id.clone(), |flow| flow.name.clone()),
            description: found.flow.as_ref().map_or(String::new(), |flow| flow.description.clone()),
            error: found.flow.as_ref().err().cloned(),
            mermaid: found.flow.as_ref().map_or(String::new(), |flow| flow.mermaid(None)),
        })
        .collect();
    let all: Vec<Run> = runs::runs(&home).into_iter().filter(|run| run.project_id == project).collect();
    let (live, finished): (Vec<&Run>, Vec<&Run>) = all.iter().partition(|run| run.status.live());
    let runs = live
        .into_iter()
        .chain(finished.into_iter().take(FINISHED_SHOWN))
        .map(|run| {
            let flow = found.iter().find(|found| found.id == run.flow_id).map(|found| found.flow.clone());
            let flow = flow.unwrap_or_else(|| Err(format!("The flow {} is gone from the project.", run.flow_id)));
            run_dto(run, flow.as_ref())
        })
        .collect();
    Ok(FlowsSnapshot { folder: model::project_dir(&root).to_string_lossy().into_owned(), flows, runs })
}

fn chat_snapshot(chat: &str) -> Result<FlowChatDto, String> {
    let home = service::process_home()?;
    Ok(FlowChatDto { run: runs::run_for_chat(&home, chat).as_ref().map(read_run) })
}

/// A user message through this window's chat host: it resumes, starts a turn, or steers one.
fn deliver(chat: &str, text: &str) -> Result<(), String> {
    let socket = crate::chat::service::backend().ensure()?;
    Client::connect(&socket)?.command(chat, ChatCommand::Send { text: text.to_owned() })
}

/// The last diagrams drawn, so a view that comes back or a run that did not move is not drawn
/// again.
fn drawn() -> &'static Mutex<VecDeque<((String, bool), FlowDiagramDto)>> {
    static DRAWN: OnceLock<Mutex<VecDeque<((String, bool), FlowDiagramDto)>>> = OnceLock::new();
    DRAWN.get_or_init(Default::default)
}

#[module]
pub struct Flows;

#[module]
impl Flows {
    /// The project's flows and runs.
    pub async fn snapshot(project: String) -> Result<FlowsSnapshot, String> {
        service::blocking(move || snapshot(&project)).await
    }

    /// The project's flows and runs now, and again whenever they change.
    pub fn subscribe(project: String, listener: Callback<FlowsSnapshot>, cx: CallCx<'_>) -> Subscription {
        Subscription::start(cx, async move {
            let Ok(changes) = service::blocking(|| Ok(watch()?.changes.subscribe())).await else { return };
            let mut last: Option<FlowsSnapshot> = None;
            while let Ok(change) = changes.recv().await {
                changes.latest(change);
                let project = project.clone();
                let next = service::blocking(move || {
                    let watch = watch()?;
                    watch.follow(&project_root(&project)?);
                    snapshot(&project)
                })
                .await;
                match next {
                    Ok(next) if last.as_ref() != Some(&next) => {
                        listener.post(next.clone());
                        last = Some(next);
                    }
                    Ok(_) => {}
                    // A project that is gone stops the watch.
                    Err(_) => break,
                }
                next_frame().await;
            }
        })
    }

    /// The chat's live run (none when no flow leads it), now and whenever it changes.
    pub fn subscribe_chat(chat: String, listener: Callback<FlowChatDto>, cx: CallCx<'_>) -> Subscription {
        Subscription::start(cx, async move {
            let Ok(changes) = service::blocking(|| Ok(watch()?.changes.subscribe())).await else { return };
            let mut last: Option<FlowChatDto> = None;
            while let Ok(change) = changes.recv().await {
                changes.latest(change);
                let chat = chat.clone();
                let next = service::blocking(move || {
                    let next = chat_snapshot(&chat)?;
                    // The run's flow file can change its step's text and ways on.
                    if let Some(run) = &next.run {
                        if let Ok(root) = project_root(&run.project_id) {
                            watch()?.follow(&root);
                        }
                    }
                    Ok(next)
                })
                .await;
                if let Ok(next) = next {
                    if last.as_ref() != Some(&next) {
                        listener.post(next.clone());
                        last = Some(next);
                    }
                }
                next_frame().await;
            }
        })
    }

    /// Creates `.ontack/flows/ID.json` from a starting flow named `name`.
    pub async fn create(project: String, name: String) -> Result<FlowCreatedDto, String> {
        service::blocking(move || {
            let (id, path) = model::create(&project_root(&project)?, &name)?;
            Ok(FlowCreatedDto { id, path: path.to_string_lossy().into_owned() })
        })
        .await
    }

    /// Leads `chat` through the flow: its first step goes to the chat as a message.
    pub async fn start(project: String, flow: String, chat: String) -> Result<FlowRunDto, String> {
        service::blocking(move || {
            let home = service::process_home()?;
            let root = project_root(&project)?;
            let socket = crate::chat::service::backend().ensure()?;
            let info = Client::connect(&socket)?
                .list()?
                .into_iter()
                .find(|info| info.id == chat)
                .ok_or("That chat is not open.")?;
            if info.project_id.as_deref() != Some(project.as_str()) {
                return Err("That chat belongs to another project.".into());
            }
            let chat = ChatRef { id: info.id, title: info.title, cwd: info.cwd };
            let run = runs::start(&home, &root, &project, &flow, &chat, &deliver)?;
            Ok(read_run(&run))
        })
        .await
    }

    /// Approves the transition the run waits for, with an optional note for the agent.
    pub async fn approve(run: String, note: String) -> Result<FlowRunDto, String> {
        service::blocking(move || Ok(read_run(&runs::approve(&service::process_home()?, &run, &note, &deliver)?))).await
    }

    /// Turns the transition down: the agent stays and is told the note.
    pub async fn deny(run: String, note: String) -> Result<FlowRunDto, String> {
        service::blocking(move || Ok(read_run(&runs::deny(&service::process_home()?, &run, &note, &deliver)?))).await
    }

    /// Ends the run; the chat is told it is no longer led.
    pub async fn stop(run: String) -> Result<FlowRunDto, String> {
        service::blocking(move || Ok(read_run(&runs::stop(&service::process_home()?, &run, &deliver)?))).await
    }

    /// Draws Mermaid source offline (as a flow's or run's `mermaid`).
    pub async fn diagram(source: String, dark: bool) -> Result<FlowDiagramDto, String> {
        service::blocking(move || {
            let key = (source, dark);
            let cache = drawn();
            if let Some((_, hit)) = cache.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(drawn, _)| drawn == &key) {
                return Ok(hit.clone());
            }
            let png = crate::diagram::render(&key.0, dark)?;
            let dto = FlowDiagramDto { data_url: png.data_url, width: png.width, height: png.height };
            let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
            cache.push_front((key, dto.clone()));
            cache.truncate(8);
            Ok(dto)
        })
        .await
    }
}
