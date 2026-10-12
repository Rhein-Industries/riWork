//! The global dashboard's picture of the work on this Mac, across every project.
//!
//! Everything here is derived from records RiWork already keeps: the store's projects,
//! worktrees and tasks, the sessions tmux has, the agent activity the window samples, the
//! chats the chat host (or, with no host running, the saved chats) knows, and the
//! automations' ledger. Nothing is estimated, and nothing reads a file or starts a process:
//! the caller samples, this only arranges. Looking never starts an agent or a chat host.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use crate::{
    activity::{ActivityCounts, AgentActivity, AgentState, ChatActivity},
    chat::model::{ChatInfo, ChatState, OrchestratorScope, Provider},
    schedules::{Outcome, Schedule, Scope},
    sessions::{HarnessKind, ShellKind, ShellSession},
    store::{State, TaskStatus},
};

/// An agent that finished its turn this recently is listed under recent work.
pub const FINISHED_WINDOW_SECS: u64 = 60 * 60;
/// A failed chat that has written nothing for this long is history, not something to do.
/// Orchestrator chats are always listed while they are failed.
pub const FAILED_CHAT_WINDOW_SECS: u64 = 3 * 24 * 60 * 60;

/// What the dashboard is drawn from. Each list is the window's own latest sample.
pub struct Inputs<'a> {
    pub state: &'a State,
    /// Every session in the registry, live or not.
    pub shells: &'a [ShellSession],
    /// Each live shell's current directory, which says which worktree it is in now.
    pub cwds: &'a BTreeMap<String, PathBuf>,
    /// What each shell's agent is doing.
    pub activity: &'a BTreeMap<String, AgentState>,
    /// Every chat, as the host or the saved chats say; `None` until they were first read.
    pub chats: Option<&'a [ChatInfo]>,
    /// When each chat's log last grew (Unix seconds), as far as it was read.
    pub chat_activity: &'a BTreeMap<String, u64>,
    /// The automations, once read; `Err` says why they could not be.
    pub schedules: Option<&'a Result<Vec<Schedule>, String>>,
    pub now: u64,
}

/// Where a click on a dashboard row goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Shell {
        id: String,
    },
    Chat {
        id: String,
        project_id: Option<String>,
        orchestrator: Option<OrchestratorScope>,
    },
    Project {
        id: String,
    },
    Task {
        id: String,
    },
    /// The Automations panel, of the schedule's project (or of the window's for an app one).
    Automations {
        project_id: Option<String>,
    },
}

/// What a session is, for its row: who runs in it and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    pub target: Target,
    /// "Codex", "Claude", "Grok", "Shell".
    pub agent: &'static str,
    /// A chat's title; shells have none of their own.
    pub title: Option<String>,
    pub project_id: Option<String>,
    /// `None` for the global scope, and for a project the store no longer has.
    pub project_name: Option<String>,
    /// The worktree's branch, when the session is in one that is not the project's root.
    pub branch: Option<String>,
    pub orchestrator: Option<OrchestratorScope>,
    pub is_chat: bool,
}

/// An agent at work, or one that recently finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Work {
    pub session: SessionSummary,
    pub activity: AgentActivity,
    /// When the activity began, when its source records it.
    pub since_unix: Option<u64>,
    pub subagents: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttentionKind {
    /// An agent waits for an answer or an approval.
    Waiting,
    /// A chat's agent failed; the next message tries to resume it.
    ChatFailed { message: String },
    /// An automation's last run did not go through.
    Automation {
        title: String,
        outcome: Outcome,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attention {
    pub kind: AttentionKind,
    pub target: Target,
    pub session: Option<SessionSummary>,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    /// When it happened, when that is known.
    pub at: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskCounts {
    pub todo: usize,
    pub in_progress: usize,
    pub done: usize,
}

impl TaskCounts {
    pub fn total(&self) -> usize {
        self.todo + self.in_progress + self.done
    }

    pub fn open(&self) -> usize {
        self.todo + self.in_progress
    }

    fn add(&mut self, status: TaskStatus) {
        match status {
            TaskStatus::Todo => self.todo += 1,
            TaskStatus::InProgress => self.in_progress += 1,
            TaskStatus::Done => self.done += 1,
        }
    }
}

/// How a project's own orchestrator runs, if it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectOrchestrator {
    Terminal {
        activity: Option<AgentActivity>,
    },
    Chat {
        provider: Provider,
        state: ChatState,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectOverview {
    pub id: String,
    pub name: String,
    /// Its group in RiWork, if it is in one.
    pub folder: Option<String>,
    pub root: PathBuf,
    /// Agents in its shells and chats, by what they are doing.
    pub agents: ActivityCounts,
    pub live_shells: usize,
    pub live_chats: usize,
    pub tasks: TaskCounts,
    pub worktrees: usize,
    /// Rows of this project in the attention list.
    pub attention: usize,
    pub orchestrator: Option<ProjectOrchestrator>,
    /// The latest thing RiWork recorded happening in it (Unix seconds): a task changed, one
    /// of its live agents began what it is doing, or one of its chats' logs grew as far as
    /// that was read. `None` when nothing recorded a time.
    pub last_activity: Option<u64>,
}

impl ProjectOverview {
    /// Nothing runs in it and nothing is open on its task list.
    pub fn is_quiet(&self) -> bool {
        self.live_shells == 0 && self.live_chats == 0 && self.tasks.open() == 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskOverview {
    pub id: String,
    pub title: String,
    pub project_id: String,
    pub project_name: String,
    pub branch: Option<String>,
    pub updated_at: u64,
}

/// The global orchestrator, as `orchestrators::find` would answer, from the same records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobalOrchestrator {
    /// The chats have not been read yet and no terminal orchestrator runs.
    Unknown,
    /// There is none. `exited_terminal` when a terminal one is registered but has ended.
    None { exited_terminal: bool },
    /// A terminal one runs; it stays the global orchestrator whatever the setting says.
    Terminal {
        shell_id: String,
        activity: Option<AgentActivity>,
    },
    Chat {
        id: String,
        provider: Provider,
        state: ChatState,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NextRun {
    pub title: String,
    pub at: u64,
    pub project_id: Option<String>,
}

/// One automation, as its row says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationSummary {
    pub id: String,
    pub title: String,
    /// `None` for an app automation.
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub paused: bool,
    /// When it runs next; a paused one has none.
    pub next_run: Option<u64>,
    /// How its last run went, and when that was seen.
    pub last_run: Option<(Outcome, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Automations {
    NotRead,
    Unreadable(String),
    Read {
        total: usize,
        paused: usize,
        next: Option<NextRun>,
        /// Every automation: the next to run first, the paused ones last.
        items: Vec<AutomationSummary>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub projects: usize,
    /// Projects with a live session, a live chat or an open task.
    pub active_projects: usize,
    pub live_shells: usize,
    pub live_chats: usize,
    pub agents: ActivityCounts,
    pub tasks: TaskCounts,
    pub attention: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overview {
    pub totals: Totals,
    /// Waiting agents first, then failures, the newest first within each.
    pub attention: Vec<Attention>,
    /// Agents working now, the longest-running first.
    pub working: Vec<Work>,
    /// Agents that finished within `FINISHED_WINDOW_SECS`, the most recent first.
    pub finished: Vec<Work>,
    /// Every project, the busiest first.
    pub projects: Vec<ProjectOverview>,
    /// Tasks in progress, the most recently updated first.
    pub tasks_in_progress: Vec<TaskOverview>,
    pub global: GlobalOrchestrator,
    pub automations: Automations,
    /// Whether the chats were read; until then chat figures are missing, not zero.
    pub chats_known: bool,
}

/// A chat whose agent process exists: it is starting, ready, or in a turn.
pub fn chat_is_live(state: &ChatState) -> bool {
    matches!(
        state,
        ChatState::Starting | ChatState::Idle | ChatState::Running | ChatState::Waiting
    )
}

/// The agent behind a shell, as its tab names it.
pub fn shell_agent(shell: &ShellSession) -> &'static str {
    match shell.harness {
        Some(HarnessKind::Codex) => "Codex",
        Some(HarnessKind::Claude) => "Claude",
        Some(HarnessKind::Grok) => "Grok",
        None if shell.kind == ShellKind::Orchestrator => "Orchestrator",
        None => "Shell",
    }
}

fn provider_agent(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "Codex",
        Provider::Claude => "Claude",
    }
}

/// The global orchestrator in the records, with `orchestrators::find`'s precedence: a live
/// terminal, else a chat, else a terminal that has ended.
pub fn global_orchestrator(
    shells: &[ShellSession],
    activity: &BTreeMap<String, AgentState>,
    chats: Option<&[ChatInfo]>,
) -> GlobalOrchestrator {
    let terminal = shells
        .iter()
        .find(|shell| shell.kind == ShellKind::Orchestrator && shell.project_id.is_none());
    if let Some(shell) = terminal.filter(|shell| shell.alive) {
        return GlobalOrchestrator::Terminal {
            shell_id: shell.id.clone(),
            activity: activity.get(&shell.id).map(|state| state.activity),
        };
    }
    let Some(chats) = chats else {
        return GlobalOrchestrator::Unknown;
    };
    match chats
        .iter()
        .find(|chat| chat.orchestrator == Some(OrchestratorScope::Global))
    {
        Some(chat) => GlobalOrchestrator::Chat {
            id: chat.id.clone(),
            provider: chat.provider,
            state: chat.state.clone(),
        },
        None => GlobalOrchestrator::None {
            exited_terminal: terminal.is_some(),
        },
    }
}

struct Names<'a> {
    state: &'a State,
    projects: HashMap<&'a str, &'a str>,
    branches: HashMap<&'a str, &'a str>,
}

impl<'a> Names<'a> {
    fn new(state: &'a State) -> Self {
        Self {
            state,
            projects: state
                .projects
                .iter()
                .map(|project| (project.id.as_str(), project.name.as_str()))
                .collect(),
            branches: state
                .worktrees
                .iter()
                .filter(|worktree| !worktree.is_primary)
                .map(|worktree| (worktree.id.as_str(), worktree.branch.as_str()))
                .collect(),
        }
    }

    fn project(&self, id: Option<&str>) -> Option<String> {
        id.and_then(|id| self.projects.get(id))
            .map(|name| (*name).to_owned())
    }

    fn branch(&self, id: Option<&str>) -> Option<String> {
        id.and_then(|id| self.branches.get(id))
            .map(|name| (*name).to_owned())
    }

    /// The worktree a shell is in now: where its directory is, else where it started.
    fn shell_branch(
        &self,
        shell: &ShellSession,
        cwds: &BTreeMap<String, PathBuf>,
    ) -> Option<String> {
        let current = cwds.get(&shell.id).and_then(|cwd| {
            self.state
                .worktrees
                .iter()
                .filter(|worktree| {
                    Some(worktree.project_id.as_str()) == shell.project_id.as_deref()
                        && cwd.starts_with(&worktree.path)
                })
                .max_by_key(|worktree| worktree.path.as_os_str().len())
        });
        match current {
            Some(worktree) if worktree.is_primary => None,
            Some(worktree) => Some(worktree.branch.clone()),
            None => self.branch(shell.worktree_id.as_deref()),
        }
    }
}

fn shell_summary(
    names: &Names,
    shell: &ShellSession,
    cwds: &BTreeMap<String, PathBuf>,
) -> SessionSummary {
    let orchestrator = (shell.kind == ShellKind::Orchestrator)
        .then(|| crate::orchestrators::scope_of(shell.project_id.as_deref()));
    SessionSummary {
        target: Target::Shell {
            id: shell.id.clone(),
        },
        agent: shell_agent(shell),
        title: None,
        project_id: shell.project_id.clone(),
        project_name: names.project(shell.project_id.as_deref()),
        branch: names.shell_branch(shell, cwds),
        orchestrator,
        is_chat: false,
    }
}

fn chat_summary(names: &Names, chat: &ChatInfo) -> SessionSummary {
    let title = chat
        .user_title
        .as_deref()
        .unwrap_or(&chat.title)
        .trim()
        .to_owned();
    SessionSummary {
        target: Target::Chat {
            id: chat.id.clone(),
            project_id: chat.project_id.clone(),
            orchestrator: chat.orchestrator.clone(),
        },
        agent: provider_agent(chat.provider),
        title: (!title.is_empty()).then_some(title),
        project_id: chat.project_id.clone(),
        project_name: names.project(chat.project_id.as_deref()),
        branch: names.branch(chat.worktree_id.as_deref()),
        orchestrator: chat.orchestrator.clone(),
        is_chat: true,
    }
}

/// Arrange the records into the dashboard.
pub fn build(inputs: &Inputs) -> Overview {
    let names = Names::new(inputs.state);
    let live_shells: Vec<&ShellSession> =
        inputs.shells.iter().filter(|shell| shell.alive).collect();
    let chats = inputs.chats.unwrap_or_default();
    let mut attention = Vec::new();
    let mut working = Vec::new();
    let mut finished = Vec::new();

    for shell in &live_shells {
        let Some(state) = inputs.activity.get(&shell.id) else {
            continue;
        };
        let work = || Work {
            session: shell_summary(&names, shell, inputs.cwds),
            activity: state.activity,
            since_unix: state.since_unix,
            subagents: state.subagents.working,
        };
        match state.activity {
            AgentActivity::Working => working.push(work()),
            AgentActivity::Waiting => attention.push(Attention {
                kind: AttentionKind::Waiting,
                target: Target::Shell {
                    id: shell.id.clone(),
                },
                session: Some(shell_summary(&names, shell, inputs.cwds)),
                project_id: shell.project_id.clone(),
                project_name: names.project(shell.project_id.as_deref()),
                at: state.since_unix,
            }),
            AgentActivity::Done
                if state.since_unix.is_some_and(|since| {
                    inputs.now.saturating_sub(since) <= FINISHED_WINDOW_SECS
                }) =>
            {
                finished.push(work())
            }
            _ => {}
        }
    }

    for chat in chats {
        match &chat.state {
            ChatState::Running => working.push(Work {
                session: chat_summary(&names, chat),
                activity: AgentActivity::Working,
                since_unix: None,
                subagents: 0,
            }),
            ChatState::Waiting => attention.push(Attention {
                kind: AttentionKind::Waiting,
                target: chat_summary(&names, chat).target,
                session: Some(chat_summary(&names, chat)),
                project_id: chat.project_id.clone(),
                project_name: names.project(chat.project_id.as_deref()),
                at: inputs.chat_activity.get(&chat.id).copied(),
            }),
            ChatState::Failed { message } => {
                let at = inputs.chat_activity.get(&chat.id).copied();
                let recent =
                    at.is_some_and(|at| inputs.now.saturating_sub(at) <= FAILED_CHAT_WINDOW_SECS);
                if chat.orchestrator.is_some() || recent {
                    attention.push(Attention {
                        kind: AttentionKind::ChatFailed {
                            message: message.clone(),
                        },
                        target: chat_summary(&names, chat).target,
                        session: Some(chat_summary(&names, chat)),
                        project_id: chat.project_id.clone(),
                        project_name: names.project(chat.project_id.as_deref()),
                        at,
                    });
                }
            }
            _ => {}
        }
    }

    let automations = match inputs.schedules {
        None => Automations::NotRead,
        Some(Err(error)) => Automations::Unreadable(error.clone()),
        Some(Ok(schedules)) => {
            for schedule in schedules.iter().filter(|schedule| !schedule.paused) {
                let Some(run) = &schedule.last_run else {
                    continue;
                };
                if !matches!(
                    run.outcome,
                    Outcome::Failed | Outcome::Missed | Outcome::Uncertain
                ) {
                    continue;
                }
                let project_id = schedule_project(&schedule.target.scope);
                attention.push(Attention {
                    kind: AttentionKind::Automation {
                        title: schedule.title.clone(),
                        outcome: run.outcome.clone(),
                        message: run.message.clone(),
                    },
                    target: Target::Automations {
                        project_id: project_id.clone(),
                    },
                    session: None,
                    project_name: names.project(project_id.as_deref()),
                    project_id,
                    at: Some(run.observed_at),
                });
            }
            let next = schedules
                .iter()
                .filter(|schedule| !schedule.paused)
                .filter_map(|schedule| schedule.next_run.map(|at| (at, schedule)))
                .min_by_key(|(at, schedule)| (*at, schedule.id.clone()))
                .map(|(at, schedule)| NextRun {
                    title: schedule.title.clone(),
                    at,
                    project_id: schedule_project(&schedule.target.scope),
                });
            let mut items: Vec<AutomationSummary> = schedules
                .iter()
                .map(|schedule| {
                    let project_id = schedule_project(&schedule.target.scope);
                    AutomationSummary {
                        id: schedule.id.clone(),
                        title: schedule.title.clone(),
                        project_name: names.project(project_id.as_deref()),
                        project_id,
                        paused: schedule.paused,
                        next_run: schedule.next_run.filter(|_| !schedule.paused),
                        last_run: schedule
                            .last_run
                            .as_ref()
                            .map(|run| (run.outcome.clone(), run.observed_at)),
                    }
                })
                .collect();
            items.sort_by(|a, b| {
                let key = |item: &AutomationSummary| {
                    (item.paused, item.next_run.is_none(), item.next_run)
                };
                key(a)
                    .cmp(&key(b))
                    .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
                    .then_with(|| a.id.cmp(&b.id))
            });
            Automations::Read {
                total: schedules.len(),
                paused: schedules.iter().filter(|schedule| schedule.paused).count(),
                next,
                items,
            }
        }
    };

    attention.sort_by_key(|item| {
        (
            !matches!(item.kind, AttentionKind::Waiting),
            Reverse(item.at.unwrap_or(0)),
        )
    });
    // The longest-running first: it is the one most worth a look.
    working.sort_by_key(|work| (work.since_unix.is_none(), work.since_unix));
    finished.sort_by_key(|work| Reverse(work.since_unix));

    // One pass over the sessions and chats, by project.
    let mut agents: HashMap<&str, ActivityCounts> = HashMap::new();
    let mut shell_counts: HashMap<&str, usize> = HashMap::new();
    let mut chat_counts: HashMap<&str, usize> = HashMap::new();
    let mut orchestrators: HashMap<&str, ProjectOrchestrator> = HashMap::new();
    for shell in &live_shells {
        let Some(project) = shell.project_id.as_deref() else {
            continue;
        };
        *shell_counts.entry(project).or_default() += 1;
        if let Some(state) = inputs.activity.get(&shell.id) {
            let counts = agents.entry(project).or_default();
            *counts = add_state(*counts, state);
        }
        if shell.kind == ShellKind::Orchestrator {
            orchestrators.insert(
                project,
                ProjectOrchestrator::Terminal {
                    activity: inputs.activity.get(&shell.id).map(|state| state.activity),
                },
            );
        }
    }
    for chat in chats {
        let Some(project) = chat.project_id.as_deref() else {
            continue;
        };
        if chat_is_live(&chat.state) {
            *chat_counts.entry(project).or_default() += 1;
        }
        if let Some(activity) = ChatActivity::of_state(&chat.state, chat.orchestrator.is_some()) {
            let counts = agents.entry(project).or_default();
            *counts = add_state(*counts, &AgentState::plain(activity));
        }
        if matches!(chat.orchestrator, Some(OrchestratorScope::Project { .. })) {
            // A live terminal wins over a chat, as `orchestrators::find` has it.
            orchestrators
                .entry(project)
                .or_insert_with(|| ProjectOrchestrator::Chat {
                    provider: chat.provider,
                    state: chat.state.clone(),
                });
        }
    }
    let mut task_counts: HashMap<&str, TaskCounts> = HashMap::new();
    for task in &inputs.state.tasks {
        task_counts
            .entry(task.project_id.as_str())
            .or_default()
            .add(task.status);
    }
    // The latest recorded time of each project: task changes, its live agents' activity,
    // and its chats' log growth as far as that was read.
    let mut last_activity: HashMap<&str, u64> = HashMap::new();
    for task in &inputs.state.tasks {
        seen(&mut last_activity, &task.project_id, task.updated_at);
    }
    for shell in &live_shells {
        if let Some(project) = shell.project_id.as_deref()
            && let Some(since) = inputs
                .activity
                .get(&shell.id)
                .and_then(|state| state.since_unix)
        {
            seen(&mut last_activity, project, since);
        }
    }
    for chat in chats {
        if let Some(project) = chat.project_id.as_deref()
            && let Some(at) = inputs.chat_activity.get(&chat.id)
        {
            seen(&mut last_activity, project, *at);
        }
    }
    let mut attention_counts: HashMap<&str, usize> = HashMap::new();
    for item in &attention {
        if let Some(project) = item.project_id.as_deref() {
            *attention_counts.entry(project).or_default() += 1;
        }
    }

    let mut projects: Vec<ProjectOverview> = inputs
        .state
        .projects
        .iter()
        .map(|project| {
            let id = project.id.as_str();
            let tasks = task_counts.get(id).copied().unwrap_or_default();
            ProjectOverview {
                id: project.id.clone(),
                name: project.name.clone(),
                folder: project.folder_id.as_deref().and_then(|folder_id| {
                    inputs
                        .state
                        .project_folders
                        .iter()
                        .find(|folder| folder.id == folder_id)
                        .map(|folder| folder.name.clone())
                }),
                root: project.root.clone(),
                agents: agents.get(id).copied().unwrap_or_default(),
                live_shells: shell_counts.get(id).copied().unwrap_or(0),
                live_chats: chat_counts.get(id).copied().unwrap_or(0),
                tasks,
                worktrees: inputs.state.worktrees_for(id).len(),
                attention: attention_counts.get(id).copied().unwrap_or(0),
                orchestrator: orchestrators.remove(id),
                last_activity: last_activity.get(id).copied(),
            }
        })
        .collect();
    projects.sort_by(|a, b| {
        let key = |project: &ProjectOverview| {
            (
                Reverse(project.attention),
                Reverse(project.agents.working),
                Reverse(project.live_shells + project.live_chats),
                Reverse(project.tasks.in_progress),
                Reverse(project.tasks.open()),
            )
        };
        key(a)
            .cmp(&key(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut tasks_in_progress: Vec<TaskOverview> = inputs
        .state
        .tasks
        .iter()
        .filter(|task| task.status == TaskStatus::InProgress)
        .filter_map(|task| {
            Some(TaskOverview {
                id: task.id.clone(),
                title: task.title.clone(),
                project_id: task.project_id.clone(),
                project_name: names.project(Some(&task.project_id))?,
                branch: names.branch(task.worktree_id.as_deref()),
                updated_at: task.updated_at,
            })
        })
        .collect();
    tasks_in_progress.sort_by_key(|task| (Reverse(task.updated_at), task.id.clone()));

    let mut totals = Totals {
        projects: projects.len(),
        active_projects: projects
            .iter()
            .filter(|project| !project.is_quiet())
            .count(),
        live_shells: live_shells.len(),
        live_chats: chats
            .iter()
            .filter(|chat| chat_is_live(&chat.state))
            .count(),
        attention: attention.len(),
        ..Totals::default()
    };
    for project in &projects {
        totals.tasks.todo += project.tasks.todo;
        totals.tasks.in_progress += project.tasks.in_progress;
        totals.tasks.done += project.tasks.done;
    }
    // Every agent counts once, global ones included, which no project row shows.
    for shell in &live_shells {
        if let Some(state) = inputs.activity.get(&shell.id) {
            totals.agents = add_state(totals.agents, state);
        }
    }
    for chat in chats {
        if let Some(activity) = ChatActivity::of_state(&chat.state, chat.orchestrator.is_some()) {
            totals.agents = add_state(totals.agents, &AgentState::plain(activity));
        }
    }

    Overview {
        totals,
        attention,
        working,
        finished,
        projects,
        tasks_in_progress,
        global: global_orchestrator(inputs.shells, inputs.activity, inputs.chats),
        automations,
        chats_known: inputs.chats.is_some(),
    }
}

/// Keep the later of what `latest` has for `project` and `at`.
fn seen<'a>(latest: &mut HashMap<&'a str, u64>, project: &'a str, at: u64) {
    let time = latest.entry(project).or_default();
    *time = (*time).max(at);
}

fn add_state(counts: ActivityCounts, state: &AgentState) -> ActivityCounts {
    let mut counts = counts;
    match state.activity {
        AgentActivity::Working => {
            counts.working += 1;
            counts.subagents += state.subagents.working;
        }
        AgentActivity::Done => counts.done += 1,
        AgentActivity::Waiting => counts.waiting += 1,
        AgentActivity::Unknown => counts.unknown += 1,
        AgentActivity::Exited => counts.exited += 1,
    }
    counts
}

fn schedule_project(scope: &Scope) -> Option<String> {
    match scope {
        Scope::App => None,
        Scope::Project { project_id } | Scope::Workspace { project_id, .. } => {
            Some(project_id.clone())
        }
    }
}

/// A duration as the dashboard writes it: "now", "45s", "12m", "3h", "2d".
pub fn short_duration(seconds: u64) -> String {
    match seconds {
        0..5 => "now".to_owned(),
        5..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

#[cfg(test)]
mod tests;
