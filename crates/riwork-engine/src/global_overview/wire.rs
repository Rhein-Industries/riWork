//! The dashboard as the phone reads it: `riwork dashboard --json`, which the remote connector
//! answers `dashboard.get` with ("Dashboard extension" in docs/remote-protocol.md).
//!
//! A projection of an `Overview`, nothing more: the same rows in the same order, cut after
//! ordering to fixed lengths, with `<list>_total` giving each list's full length. A figure
//! that is not known is left out, never written as zero: without the chats there is no
//! `live_chats`, and a time nothing recorded is absent. Optional keys are omitted, never
//! `null`. Text is cut to a fixed number of Unicode scalar values, never inside one.
//! No project root path, no model and no executable path are part of it.
//!
//! `backends` was added to version 1 additively: a reader from before it ignores the key, and
//! a reader of it takes a reply without it as not read.

use serde_json::{Map, Value};

use super::{
    ActivityCounts, AgentActivity, AppOverview, Attention, AttentionKind, AutomationSummary,
    Automations, Backends, ChatHostOverview, ChatState, CliState, GlobalOrchestrator, Outcome,
    Overview, ProjectOrchestrator, ProjectOverview, Provider, ProviderOverview, SessionSummary,
    Target, TaskCounts, TaskOverview, Work,
};
use crate::chat::model::OrchestratorScope;

/// The version of this shape. A reader refuses any other.
pub const WIRE_VERSION: u64 = 1;
/// The most rows of each list the wire carries; `<list>_total` is the full length.
pub const WIRE_MAX_ATTENTION: usize = 40;
pub const WIRE_MAX_WORKING: usize = 40;
pub const WIRE_MAX_FINISHED: usize = 20;
pub const WIRE_MAX_PROJECTS: usize = 150;
pub const WIRE_MAX_TASKS_IN_PROGRESS: usize = 50;
pub const WIRE_MAX_AUTOMATIONS: usize = 30;
/// `backends.providers`: every provider RiWork knows fits.
pub const WIRE_MAX_PROVIDERS: usize = 8;
/// Titles, names, branches and folders: at most this many Unicode scalar values.
pub const WIRE_MAX_NAME_CHARS: usize = 120;
/// Failure and automation messages: at most this many Unicode scalar values.
pub const WIRE_MAX_MESSAGE_CHARS: usize = 300;

/// The dashboard `overview` as the wire carries it, sampled at `generated_at` (Unix seconds).
pub fn wire(overview: &Overview, generated_at: u64) -> Value {
    let known = overview.chats_known;
    let mut root = Object::new()
        .set("v", WIRE_VERSION)
        .set("generated_at", generated_at)
        .set("chats_known", known)
        .set("totals", totals(overview))
        .set("global", global(&overview.global));
    root = root.list(
        "attention",
        &overview.attention,
        WIRE_MAX_ATTENTION,
        attention,
    );
    root = root.list("working", &overview.working, WIRE_MAX_WORKING, work);
    root = root.list("finished", &overview.finished, WIRE_MAX_FINISHED, work);
    root = root.list("projects", &overview.projects, WIRE_MAX_PROJECTS, |p| {
        project(p, known)
    });
    root = root.list(
        "tasks_in_progress",
        &overview.tasks_in_progress,
        WIRE_MAX_TASKS_IN_PROGRESS,
        task,
    );
    root.set("automations", automations(&overview.automations))
        .set("backends", backends(&overview.backends))
        .done()
}

/// `text` cut to at most `max` Unicode scalar values.
fn bounded(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => text[..end].to_owned(),
        None => text.to_owned(),
    }
}

fn name(text: &str) -> String {
    bounded(text, WIRE_MAX_NAME_CHARS)
}

fn message(text: &str) -> String {
    bounded(text, WIRE_MAX_MESSAGE_CHARS)
}

/// A JSON object that leaves out what is not known.
struct Object(Map<String, Value>);

impl Object {
    fn new() -> Self {
        Self(Map::new())
    }

    fn set(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.0.insert(key.to_owned(), value.into());
        self
    }

    fn maybe(self, key: &str, value: Option<impl Into<Value>>) -> Self {
        match value {
            Some(value) => self.set(key, value),
            None => self,
        }
    }

    /// The first `cap` of `items` under `key`, and how many there are under `key_total`.
    fn list<T>(self, key: &str, items: &[T], cap: usize, row: impl Fn(&T) -> Value) -> Self {
        let rows: Vec<Value> = items.iter().take(cap).map(row).collect();
        self.set(key, rows)
            .set(&format!("{key}_total"), items.len())
    }

    fn done(self) -> Value {
        Value::Object(self.0)
    }
}

fn agents(counts: &ActivityCounts) -> Value {
    Object::new()
        .set("working", counts.working)
        .set("waiting", counts.waiting)
        .set("done", counts.done)
        .set("unknown", counts.unknown)
        .set("exited", counts.exited)
        .set("subagents", counts.subagents)
        .done()
}

fn tasks(counts: &TaskCounts) -> Value {
    Object::new()
        .set("todo", counts.todo)
        .set("in_progress", counts.in_progress)
        .set("done", counts.done)
        .done()
}

fn totals(overview: &Overview) -> Value {
    let totals = &overview.totals;
    Object::new()
        .set("projects", totals.projects)
        .set("active_projects", totals.active_projects)
        .set("live_shells", totals.live_shells)
        .maybe(
            "live_chats",
            overview.chats_known.then_some(totals.live_chats),
        )
        .set("agents", agents(&totals.agents))
        .set("tasks", tasks(&totals.tasks))
        .set("attention", totals.attention)
        .done()
}

fn provider(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "codex",
        Provider::Claude => "claude",
        Provider::Muse => "muse",
    }
}

fn chat_state(state: &ChatState) -> &'static str {
    match state {
        ChatState::Starting => "starting",
        ChatState::Idle => "idle",
        ChatState::Running => "running",
        ChatState::Waiting => "waiting",
        ChatState::Stopped => "stopped",
        ChatState::Failed { .. } => "failed",
    }
}

/// What a failed chat said, bounded; nothing for a chat that did not fail.
fn chat_failure(state: &ChatState) -> Option<String> {
    match state {
        ChatState::Failed { message: text } => Some(message(text)),
        _ => None,
    }
}

fn activity(activity: AgentActivity) -> &'static str {
    activity.as_str()
}

fn outcome(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Dispatching => "dispatching",
        Outcome::Submitted => "submitted",
        Outcome::Deferred => "deferred",
        Outcome::Missed => "missed",
        Outcome::Failed => "failed",
        Outcome::Uncertain => "uncertain",
    }
}

fn scope(scope: &OrchestratorScope) -> &'static str {
    match scope {
        OrchestratorScope::Global => "global",
        OrchestratorScope::Project { .. } => "project",
    }
}

fn global(global: &GlobalOrchestrator) -> Value {
    match global {
        GlobalOrchestrator::Unknown => Object::new().set("kind", "unknown").done(),
        GlobalOrchestrator::None { exited_terminal } => Object::new()
            .set("kind", "none")
            .set("exited_terminal", *exited_terminal)
            .done(),
        GlobalOrchestrator::Terminal {
            shell_id,
            activity: state,
        } => Object::new()
            .set("kind", "terminal")
            .set("shell_id", shell_id.as_str())
            .maybe("activity", state.map(activity))
            .done(),
        GlobalOrchestrator::Chat {
            id,
            provider: chat_provider,
            state,
        } => Object::new()
            .set("kind", "chat")
            .set("chat_id", id.as_str())
            .set("provider", provider(*chat_provider))
            .set("chat_state", chat_state(state))
            .maybe("message", chat_failure(state))
            .done(),
    }
}

fn target(target: &Target) -> Value {
    match target {
        Target::Shell { id } => Object::new()
            .set("type", "shell")
            .set("id", id.as_str())
            .done(),
        Target::Chat {
            id,
            project_id,
            orchestrator,
        } => Object::new()
            .set("type", "chat")
            .set("id", id.as_str())
            .maybe("project_id", project_id.as_deref())
            .maybe("orchestrator", orchestrator.as_ref().map(scope))
            .done(),
        Target::Project { id } => Object::new()
            .set("type", "project")
            .set("id", id.as_str())
            .done(),
        Target::Task { id } => Object::new()
            .set("type", "task")
            .set("id", id.as_str())
            .done(),
        Target::Automations { project_id } => Object::new()
            .set("type", "automations")
            .maybe("project_id", project_id.as_deref())
            .done(),
    }
}

fn session(session: &SessionSummary) -> Value {
    Object::new()
        .set("target", target(&session.target))
        .set("agent", session.agent)
        .maybe("title", session.title.as_deref().map(name))
        .maybe("project_id", session.project_id.as_deref())
        .maybe("project_name", session.project_name.as_deref().map(name))
        .maybe("branch", session.branch.as_deref().map(name))
        .maybe("orchestrator", session.orchestrator.as_ref().map(scope))
        .set("is_chat", session.is_chat)
        .done()
}

fn work(work: &Work) -> Value {
    Object::new()
        .set("session", session(&work.session))
        .set("activity", activity(work.activity))
        .maybe("since_unix", work.since_unix)
        .set("subagents", work.subagents)
        .done()
}

fn attention(item: &Attention) -> Value {
    let kind = match item.kind {
        AttentionKind::Waiting => "waiting",
        AttentionKind::ChatFailed { .. } => "chat_failed",
        AttentionKind::Automation { .. } => "automation",
    };
    let row = Object::new()
        .set("kind", kind)
        .set("target", target(&item.target))
        .maybe("session", item.session.as_ref().map(session))
        .maybe("project_id", item.project_id.as_deref())
        .maybe("project_name", item.project_name.as_deref().map(name))
        .maybe("at", item.at);
    match &item.kind {
        AttentionKind::Waiting => row,
        AttentionKind::ChatFailed { message: text } => row.set("message", message(text)),
        AttentionKind::Automation {
            title,
            outcome: run,
            message: text,
        } => row
            .set("message", message(text))
            .set("title", name(title))
            .set("outcome", outcome(run)),
    }
    .done()
}

fn project(project: &ProjectOverview, chats_known: bool) -> Value {
    let orchestrator = project
        .orchestrator
        .as_ref()
        .map(|orchestrator| match orchestrator {
            ProjectOrchestrator::Terminal { activity: state } => Object::new()
                .set("mode", "terminal")
                .maybe("activity", state.map(activity))
                .done(),
            ProjectOrchestrator::Chat {
                provider: chat_provider,
                state,
            } => Object::new()
                .set("mode", "chat")
                .set("provider", provider(*chat_provider))
                .set("chat_state", chat_state(state))
                .maybe("message", chat_failure(state))
                .done(),
        });
    Object::new()
        .set("id", project.id.as_str())
        .set("name", name(&project.name))
        .maybe("folder", project.folder.as_deref().map(name))
        .set("agents", agents(&project.agents))
        .set("live_shells", project.live_shells)
        .maybe("live_chats", chats_known.then_some(project.live_chats))
        .set("tasks", tasks(&project.tasks))
        .set("worktrees", project.worktrees)
        .set("attention", project.attention)
        .maybe("orchestrator", orchestrator)
        .maybe("last_activity", project.last_activity)
        .done()
}

fn task(task: &TaskOverview) -> Value {
    Object::new()
        .set("id", task.id.as_str())
        .set("title", name(&task.title))
        .set("project_id", task.project_id.as_str())
        .set("project_name", name(&task.project_name))
        .maybe("branch", task.branch.as_deref().map(name))
        .set("updated_at", task.updated_at)
        .done()
}

fn automation(item: &AutomationSummary) -> Value {
    Object::new()
        .set("id", item.id.as_str())
        .set("title", name(&item.title))
        .maybe("project_id", item.project_id.as_deref())
        .maybe("project_name", item.project_name.as_deref().map(name))
        .set("paused", item.paused)
        .maybe("next_run", item.next_run)
        .maybe(
            "last_run",
            item.last_run.as_ref().map(|(run, at)| {
                Object::new()
                    .set("outcome", outcome(run))
                    .set("at", *at)
                    .done()
            }),
        )
        .done()
}

fn automations(automations: &Automations) -> Value {
    match automations {
        Automations::NotRead => Object::new().set("state", "not_read").done(),
        Automations::Unreadable(text) => Object::new()
            .set("state", "unreadable")
            .set("message", message(text))
            .done(),
        Automations::Read {
            total,
            paused,
            next,
            items,
        } => Object::new()
            .set("state", "read")
            .set("total", *total)
            .set("paused", *paused)
            .maybe(
                "next",
                next.as_ref().map(|next| {
                    Object::new()
                        .set("title", name(&next.title))
                        .set("at", next.at)
                        .maybe("project_id", next.project_id.as_deref())
                        .done()
                }),
            )
            .list("items", items, WIRE_MAX_AUTOMATIONS, automation)
            .done(),
    }
}

fn app(app: &AppOverview) -> Value {
    Object::new()
        .set("running", app.running)
        .set("windows", app.windows)
        .maybe("started_at", app.started_at)
        .maybe("build_at", app.build_at)
        .maybe(
            "freshness",
            app.freshness.map(|freshness| freshness.as_str()),
        )
        .done()
}

fn chat_host(host: &ChatHostOverview) -> Value {
    match host {
        ChatHostOverview::Stopped => Object::new().set("state", "stopped").done(),
        ChatHostOverview::Unreadable(text) => Object::new()
            .set("state", "unreadable")
            .set("message", message(text))
            .done(),
        ChatHostOverview::Running {
            pid,
            started_at,
            build_at,
            freshness,
        } => Object::new()
            .set("state", "running")
            .set("pid", *pid)
            .maybe("started_at", *started_at)
            .maybe("build_at", *build_at)
            .set("freshness", freshness.as_str())
            .done(),
    }
}

fn provider_row(row: &ProviderOverview) -> Value {
    let cli = match &row.cli {
        CliState::Found { .. } => "found",
        CliState::Missing => "missing",
        CliState::Unknown => "unknown",
    };
    let (version, replaced_at) = match &row.cli {
        CliState::Found {
            version,
            replaced_at,
        } => (version.as_deref().map(name), *replaced_at),
        CliState::Missing | CliState::Unknown => (None, None),
    };
    Object::new()
        .set("provider", row.provider.program())
        .set("cli", cli)
        .maybe("version", version)
        .maybe("replaced_at", replaced_at)
        .set("shells", row.shells)
        .maybe("chats", row.chats)
        .maybe("running", row.running())
        .maybe("oldest_start", row.oldest_start)
        .set("stale", row.stale)
        .set("unknown", row.unknown)
        .done()
}

fn backends(backends: &Backends) -> Value {
    match backends {
        Backends::NotRead => Object::new().set("state", "not_read").done(),
        Backends::Read(read) => Object::new()
            .set("state", "read")
            .set("app", app(&read.app))
            .set("chat_host", chat_host(&read.chat_host))
            .list(
                "providers",
                &read.providers,
                WIRE_MAX_PROVIDERS,
                provider_row,
            )
            .done(),
    }
}
