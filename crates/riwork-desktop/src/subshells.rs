//! Workspace integration: reuse the inventory sample and the existing shell operations.
//! Each chat tab gets the live state of its own workers (those whose `parent_id` is that
//! chat), and the host hears when a worker finished or was found without a card, so the
//! parent chat's timeline keeps the record (`chat::workers`).
use crate::{
    Workspace, activity,
    chat::workers::{self, LiveStatus, LiveWorker, WorkerEnd, WorkerFinal, WorkerKind, WorkerNote},
    chat_view::ShellAction,
    sessions::{SessionManager, ShellSession},
};
use gpui::{Context, Window};
use notify::{RecursiveMode, Watcher};
use riwork_engine::subshells::{PollState, ShellCard, bounded_output};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    time::Instant,
};

/// A live state older than this is labelled on running cards.
const STALE_AFTER: u64 = 30;

/// One thing a parent chat's host is told about a worker.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Said {
    /// Its start may have been missed: a late card, unless it has one.
    Late(String),
    /// It became done at this time.
    Done(String, u64),
    Exited(String),
}

impl WorkerState {
    /// The notes for what changed since the last call, each with what it says. `fresh`:
    /// the terminals were just sampled, so a done can go with its last lines.
    fn transitions(
        &mut self,
        shells: &[ShellSession],
        activity: &BTreeMap<String, activity::AgentState>,
        project: &str,
        now: u64,
        fresh: bool,
    ) -> Vec<(String, WorkerNote, Said)> {
        let mut notes = Vec::new();
        for shell in shells
            .iter()
            .filter(|s| s.is_worker() && s.project_id.as_deref() == Some(project))
        {
            let Some(parent) = shell.parent_id.clone() else {
                continue;
            };
            self.seen.insert(shell.id.clone(), parent.clone());
            // Dead: closed below, even if it ended while no app watched it.
            if !shell.alive {
                continue;
            }
            if now.saturating_sub(shell.created_at_unix) >= workers::LATE_AFTER
                && self.said.insert(Said::Late(shell.id.clone()))
            {
                // A done said before there was a card ended nothing: said again for this one.
                self.said
                    .retain(|said| !matches!(said, Said::Done(id, _) if id == &shell.id));
                notes.push((
                    parent.clone(),
                    WorkerNote::Spawn {
                        session_id: shell.id.clone(),
                        kind: WorkerKind::Shell,
                        late: true,
                    },
                    Said::Late(shell.id.clone()),
                ));
            }
            if let Some(state) = activity.get(&shell.id)
                && state.activity == activity::AgentActivity::Done
                && let Some(since) = state.since_unix
                && !self.said.contains(&Said::Done(shell.id.clone(), since))
            {
                self.awaiting.insert((shell.id.clone(), since), parent);
            }
        }
        if fresh {
            // Only a terminal this sample read has its last lines from after it was done;
            // one that could not be read waits for a later check. One that is gone is
            // recorded as exited, below, with the last lines that could be read.
            self.awaiting.retain(|(id, _), _| {
                shells
                    .iter()
                    .any(|shell| &shell.id == id && shell.alive && shell.is_worker())
            });
            let (read, unread): (BTreeMap<_, _>, BTreeMap<_, _>) =
                std::mem::take(&mut self.awaiting)
                    .into_iter()
                    .partition(|((id, _), _)| matches!(self.outputs.get(id), Some(Ok(_))));
            self.awaiting = unread;
            for ((id, since), parent) in read {
                if self.said.insert(Said::Done(id.clone(), since)) {
                    notes.push((
                        parent,
                        WorkerNote::Finish {
                            session_id: id.clone(),
                            kind: WorkerKind::Shell,
                            end: WorkerFinal {
                                status: WorkerEnd::Done,
                                ended_at: since,
                                tail: self.last_output.get(&id).cloned().unwrap_or_default(),
                                turn: None,
                                turn_end: None,
                                done_since: None,
                            },
                            done_since: Some(since),
                            turn: None,
                            turn_end: None,
                            took: Vec::new(),
                        },
                        Said::Done(id, since),
                    ));
                }
            }
        } else if !self.awaiting.is_empty() {
            // The next check reads their screens.
            self.poll.request();
        }
        let exited: Vec<(String, String)> = self
            .seen
            .iter()
            .filter(|(id, _)| !shells.iter().any(|shell| &shell.id == *id && shell.alive))
            .map(|(id, parent)| (id.clone(), parent.clone()))
            .collect();
        for (id, parent) in exited {
            if self.said.insert(Said::Exited(id.clone())) {
                notes.push((
                    parent,
                    WorkerNote::Finish {
                        session_id: id.clone(),
                        kind: WorkerKind::Shell,
                        end: WorkerFinal {
                            status: WorkerEnd::Exited,
                            ended_at: now,
                            tail: self.last_output.get(&id).cloned().unwrap_or_default(),
                            turn: None,
                            turn_end: None,
                            done_since: None,
                        },
                        done_since: None,
                        turn: None,
                        turn_end: None,
                        took: Vec::new(),
                    },
                    Said::Exited(id),
                ));
            }
        }
        notes
    }

    /// After a batch went out: what did not reach a host is said again at a later check,
    /// which goes on while there is any; a recorded exit needs no more watching.
    fn settle(&mut self, sent: &[Said], undelivered: &[Said]) {
        for said in sent {
            if undelivered.contains(said) {
                self.said.remove(said);
                self.undelivered.insert(said.clone());
            } else {
                self.undelivered.remove(said);
                if let Said::Exited(id) = said {
                    self.seen.remove(id);
                }
            }
        }
    }

    /// Whether a note is still to be said again: checks go on for it.
    fn retrying(&self) -> bool {
        !self.undelivered.is_empty()
    }
}

pub(crate) struct WorkerState {
    pub poll: PollState,
    pub models: BTreeMap<String, String>,
    clock: Instant,
    scope: String,
    outputs: BTreeMap<String, Result<String, String>>,
    /// Each worker's last lines that could be read, for its card and its recorded end.
    last_output: BTreeMap<String, String>,
    /// Workers with a parent seen in this project, by id: their parent chat. One that is
    /// gone or dead is recorded as exited, with the last lines kept for it until then.
    seen: BTreeMap<String, String>,
    /// What the host was told in this run, so each is said once.
    said: HashSet<Said>,
    /// Dones waiting for the next sample of their terminal, so their record has its last
    /// lines: the worker and when it became done, with its parent chat.
    awaiting: BTreeMap<(String, u64), String>,
    /// Notes that did not reach a host, until one says them: checks go on meanwhile.
    undelivered: HashSet<Said>,
    watcher: Option<notify::RecommendedWatcher>,
    watch_error: Option<String>,
    fingerprint: Vec<Option<(std::time::SystemTime, u64)>>,
    paused: bool,
    generation: u64,
}
impl Default for WorkerState {
    fn default() -> Self {
        Self {
            poll: Default::default(),
            models: BTreeMap::new(),
            clock: Instant::now(),
            scope: String::new(),
            outputs: BTreeMap::new(),
            last_output: BTreeMap::new(),
            seen: BTreeMap::new(),
            said: HashSet::new(),
            awaiting: BTreeMap::new(),
            undelivered: HashSet::new(),
            watcher: None,
            watch_error: None,
            fingerprint: Vec::new(),
            paused: false,
            generation: 0,
        }
    }
}

fn relevant(path: &Path, home: &Path) -> bool {
    path == home.join("sessions.json")
        || path == home.join("orchestrator-wakes.json")
        || path.starts_with(home.join("agent-activity"))
}

fn fingerprint(home: &Path) -> Vec<Option<(std::time::SystemTime, u64)>> {
    ["sessions.json", "orchestrator-wakes.json", "agent-activity"]
        .iter()
        .map(|path| {
            let metadata = std::fs::metadata(home.join(path)).ok()?;
            Some((metadata.modified().ok()?, metadata.len()))
        })
        .collect()
}

pub(crate) fn capture_workers(
    sessions: &SessionManager,
    shells: &[ShellSession],
    project: &str,
) -> BTreeMap<String, Result<String, String>> {
    shells
        .iter()
        .filter(|s| s.alive && s.is_worker() && s.project_id.as_deref() == Some(project))
        .map(|s| {
            (
                s.id.clone(),
                sessions
                    .capture_sampled(s, 6)
                    .map(|text| bounded_output(&text)),
            )
        })
        .collect()
}

trait WorkerOperations {
    fn get_worker(&self, id: &str) -> Result<ShellSession, String>;
    fn send_existing(&self, id: &str, text: &str) -> Result<(), String>;
    fn close_existing(&self, id: &str) -> Result<(), String>;
}
impl WorkerOperations for SessionManager {
    fn get_worker(&self, id: &str) -> Result<ShellSession, String> {
        self.get(id)
    }
    fn send_existing(&self, id: &str, text: &str) -> Result<(), String> {
        self.send(id, text)
    }
    fn close_existing(&self, id: &str) -> Result<(), String> {
        self.close(id)
    }
}
/// Same input/close path as the CLI. The test seam never starts or addresses tmux.
fn dispatch(
    sessions: &impl WorkerOperations,
    project: &str,
    id: &str,
    action: &ShellAction,
) -> Result<(), String> {
    let shell = sessions.get_worker(id)?;
    if shell.project_id.as_deref() != Some(project) || !shell.is_worker() {
        return Err("This worker no longer belongs to this chat's project.".into());
    }
    match action {
        ShellAction::Send(text) => sessions.send_existing(id, text),
        ShellAction::Stop => sessions.close_existing(id),
        ShellAction::Open => Ok(()),
    }
}

impl Workspace {
    pub(crate) fn start_worker_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let home = self.sessions.state_home().to_path_buf();
        let filter_home = home.clone();
        let (tx, rx) = async_channel::bounded(1);
        let watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
                Err(error) => {
                    let _ = tx.try_send(Some(error.to_string()));
                }
                Ok(event) if event.paths.iter().any(|path| relevant(path, &filter_home)) => {
                    let _ = tx.try_send(None);
                }
                _ => {}
            })
            .and_then(|mut watcher| {
                watcher.watch(&home, RecursiveMode::Recursive)?;
                Ok(watcher)
            });
        match watcher {
            Ok(watcher) => self.subshells.watcher = Some(watcher),
            Err(error) => {
                self.subshells.watch_error = Some(format!("Event watcher unavailable: {error}"))
            }
        }
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(error) = rx.recv().await {
                if this
                    .update_in(cx, |workspace, window, cx| {
                        if let Some(error) = error {
                            workspace.subshells.watch_error = Some(format!(
                                "Event watcher failed: {error}; discovery checks every 2s"
                            ));
                        }
                        workspace.request_worker_refresh(window, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn request_worker_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.subshells.poll.request();
        self.sessions.invalidate_sample();
        self.refresh_sessions(window, cx);
    }

    pub(crate) fn begin_worker_sample(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.subshells.watch_error.is_some() {
            let fingerprint = fingerprint(self.sessions.state_home());
            if self.subshells.fingerprint != fingerprint {
                self.subshells.fingerprint = fingerprint;
                self.subshells.poll.request();
                self.sessions.invalidate_sample();
            }
        }
        if self.subshells.scope != self.project_id {
            self.subshells.scope = self.project_id.clone();
            self.subshells.poll = Default::default();
            self.subshells.outputs.clear();
            self.subshells.last_output.clear();
            // Another project's workers are not gone because this window looks elsewhere,
            // and its dones wait until it is looked at again.
            self.subshells.seen.clear();
            self.subshells.awaiting.clear();
        }
        let paused = self.is_remote() || !window.is_visible() || cx.active_window().is_none();
        if self.subshells.paused && !paused
            || self.subshells.generation != self.session_refresh_generation
        {
            self.subshells.poll.request();
            self.sessions.invalidate_sample();
        }
        self.subshells.paused = paused;
        self.subshells.generation = self.session_refresh_generation;
        let live = self.worker_checks_wanted();
        let sample =
            self.subshells
                .poll
                .begin(self.subshells.clock.elapsed().as_secs(), live, paused);
        self.publish_workers(cx);
        sample
    }

    /// Live shells to watch, or notes to say again.
    fn worker_checks_wanted(&self) -> bool {
        self.worker_sample_live() || self.subshells.retrying()
    }

    fn worker_sample_live(&self) -> bool {
        self.shells
            .iter()
            .any(|s| s.alive && s.project_id.as_deref() == Some(self.project_id.as_str()))
            || self
                .panes
                .values()
                .flat_map(|p| p.tabs.iter())
                .filter_map(|tab| tab.shell_id())
                .any(|id| self.shells.iter().any(|s| s.id == id && s.alive))
    }

    pub(crate) fn finish_worker_sample(
        &mut self,
        outputs: BTreeMap<String, Result<String, String>>,
        changed: bool,
        succeeded: bool,
        cx: &mut Context<Self>,
    ) {
        let output_changed = outputs
            .iter()
            .any(|(id, result)| self.subshells.outputs.get(id) != Some(result));
        for (id, output) in &outputs {
            if let Ok(text) = output {
                self.subshells.last_output.insert(id.clone(), text.clone());
            }
        }
        self.subshells.outputs.extend(outputs);
        self.subshells.poll.finish(
            self.subshells.clock.elapsed().as_secs(),
            changed || output_changed,
            succeeded,
            self.worker_checks_wanted(),
        );
        self.publish_workers(cx);
        if succeeded {
            self.note_worker_transitions(true, cx);
        }
    }

    /// The live state of this project's workers, with the chat that started each.
    fn live_workers(&self) -> Vec<(String, String, LiveWorker)> {
        let project = Some(self.project_id.as_str());
        let shells = self
            .shells
            .iter()
            .filter(|s| s.is_worker() && s.project_id.as_deref() == project)
            .filter_map(|shell| {
                let parent = shell.parent_id.clone()?;
                let card =
                    ShellCard::of(shell, self.agent_activity.get(&shell.id), &self.state.tasks);
                let output_error = match self.subshells.outputs.get(&shell.id) {
                    Some(Err(error)) => Some(bounded_output(error)),
                    _ => None,
                };
                Some((
                    shell.id.clone(),
                    parent,
                    LiveWorker {
                        status: LiveStatus::of_shell(&card),
                        actionable: shell.alive,
                        model: self.subshells.models.get(&shell.id).cloned(),
                        output: self
                            .subshells
                            .last_output
                            .get(&shell.id)
                            .cloned()
                            .unwrap_or_default(),
                        output_error,
                    },
                ))
            });
        let chats = self
            .shared_chats
            .iter()
            .flatten()
            .filter(|chat| chat.project_id.as_deref() == project)
            .filter_map(|chat| {
                Some((
                    chat.id.clone(),
                    chat.parent_id.clone()?,
                    LiveWorker {
                        status: LiveStatus::of_chat(&chat.state),
                        actionable: false,
                        model: chat.model.clone(),
                        output: String::new(),
                        output_error: None,
                    },
                ))
            });
        shells.chain(chats).collect()
    }

    pub(crate) fn publish_workers(&mut self, cx: &mut Context<Self>) {
        self.subshells
            .outputs
            .retain(|id, _| self.shells.iter().any(|shell| &shell.id == id));
        // A worker that went keeps its last lines until its exit is recorded.
        self.subshells.last_output.retain(|id, _| {
            self.shells.iter().any(|shell| &shell.id == id) || self.subshells.seen.contains_key(id)
        });
        let live = self.live_workers();
        let now = self.subshells.clock.elapsed().as_secs();
        // Said on running cards only when what they show may be old.
        let stale = (self.subshells.paused
            || self.sessions_sample_error.is_some()
            || self.subshells.watch_error.is_some()
            || self
                .subshells
                .poll
                .last_checked
                .is_none_or(|at| now.saturating_sub(at) > STALE_AFTER))
        .then(|| {
            let mut label = self.subshells.poll.label(
                now,
                self.worker_sample_live(),
                self.subshells.paused,
                self.sessions_sample_error.as_deref(),
            );
            if let Some(error) = &self.subshells.watch_error {
                label.push_str(" · ");
                label.push_str(error);
            }
            label
        });
        let project = self.project_id.clone();
        for (chat_id, view) in self
            .panes
            .values()
            .flat_map(|p| p.tabs.iter())
            .filter_map(|tab| Some((tab.chat_id()?.to_owned(), tab.chat()?.clone())))
            .collect::<Vec<_>>()
        {
            let belongs = view.read(cx).summary().project_id.as_deref() == Some(project.as_str());
            let mine = if belongs {
                workers_of_chat(&live, &chat_id)
            } else {
                BTreeMap::new()
            };
            view.update(cx, |view, cx| view.set_subshells(mine, stale.clone(), cx));
        }
    }

    /// Tells the host what changed for the record in a parent chat: a worker that became
    /// done or exited, and (once, after `workers::LATE_AFTER`) a worker whose start it may
    /// have missed. `fresh`: the terminals were just sampled.
    fn note_worker_transitions(&mut self, fresh: bool, cx: &mut Context<Self>) {
        if self.is_remote() {
            return;
        }
        let notes = self.subshells.transitions(
            &self.shells,
            &self.agent_activity,
            &self.project_id,
            activity::unix_now(),
            fresh,
        );
        if notes.is_empty() {
            return;
        }
        let sent: Vec<Said> = notes.iter().map(|(_, _, said)| said.clone()).collect();
        let home = self.sessions.state_home().to_path_buf();
        let work = cx
            .background_executor()
            .spawn(async move { deliver_notes(&home, notes) });
        cx.spawn(async move |this, cx| {
            let undelivered = work.await;
            let _ = this.update(cx, |workspace, _| {
                workspace.subshells.settle(&sent, &undelivered)
            });
        })
        .detach();
    }

    pub(crate) fn worker_activity_changed(&mut self, cx: &mut Context<Self>) {
        self.subshells.poll.note_change(
            self.subshells.clock.elapsed().as_secs(),
            self.worker_sample_live(),
        );
        self.publish_workers(cx);
        self.note_worker_transitions(false, cx);
    }

    pub(crate) fn worker_refresh_requested(&self) -> bool {
        self.subshells.poll.has_request() && !self.subshells.paused
    }

    pub(crate) fn worker_chat_event(
        &mut self,
        view: &gpui::Entity<crate::chat_view::ChatView>,
        id: &str,
        action: &ShellAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action == &ShellAction::Open
            && !self.is_remote()
            && let Some(chat) = self
                .shared_chats
                .iter()
                .flatten()
                .find(|chat| chat.id == id && chat.parent_id.is_some())
                .cloned()
        {
            self.show_chat_with_config(chat, self.chat_config(), None, window, cx);
            return;
        }
        if self.is_remote()
            || view.read(cx).summary().project_id.as_deref() != Some(self.project_id.as_str())
            || !self.shells.iter().any(|s| {
                s.id == id
                    && s.is_worker()
                    && s.project_id.as_deref() == Some(self.project_id.as_str())
            })
        {
            view.update(cx, |view, cx| {
                view.subshell_action_finished(
                    id,
                    Err("This worker is no longer available in this chat's project.".into()),
                    cx,
                )
            });
            return;
        }
        if action == &ShellAction::Open {
            self.show_shell(id, window, cx);
            self.request_worker_refresh(window, cx);
            return;
        }
        let sessions = self.sessions.clone();
        let id = id.to_owned();
        let project = self.project_id.clone();
        let action = action.clone();
        let view = view.downgrade();
        let work_id = id.clone();
        let work_project = project.clone();
        let work = cx
            .background_executor()
            .spawn(async move { dispatch(&sessions, &work_project, &work_id, &action) });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |workspace, window, cx| {
                let _ = view.update(cx, |view, cx| {
                    view.subshell_action_finished(&id, result, cx)
                });
                workspace.session_refresh_generation =
                    workspace.session_refresh_generation.wrapping_add(1);
                workspace.request_worker_refresh(window, cx);
            });
        })
        .detach();
    }
}

/// The workers a chat started, out of its project's: never another chat's.
fn workers_of_chat(
    live: &[(String, String, LiveWorker)],
    chat_id: &str,
) -> BTreeMap<String, LiveWorker> {
    live.iter()
        .filter(|(_, parent, _)| parent == chat_id)
        .map(|(id, _, worker)| (id.clone(), worker.clone()))
        .collect()
}

/// Sends each note to the host of the parent chat. A parent that is not a chat has no
/// timeline; a host that refuses a note recorded nothing and is not asked again. Returns
/// the notes that found no host or whose exchange broke, to be said again on a later
/// check: the host records each end once.
fn deliver_notes(home: &Path, notes: Vec<(String, WorkerNote, Said)>) -> Vec<Said> {
    use crate::chat::client::{CallError, Client};
    let socket = crate::chat::client::socket_path(home);
    notes
        .into_iter()
        .filter(|(parent, _, _)| workers::parent_is_chat(home, parent))
        .filter_map(|(parent, note, key)| {
            match Client::connect(&socket).map(|mut client| client.note_worker(&parent, note)) {
                Ok(Ok(()) | Err(CallError::Refused(_))) => None,
                Ok(Err(CallError::Broken(_))) | Err(_) => Some(key),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn worker_shell(id: &str, parent: Option<&str>, alive: bool, created: u64) -> ShellSession {
        let mut shell: ShellSession = serde_json::from_value(serde_json::json!({
            "id": id, "parent_id": parent, "project_id": "p", "kind": "project",
            "cwd": "/fixture", "command": null, "harness": "claude", "created_at_unix": created
        }))
        .unwrap();
        shell.alive = alive;
        shell
    }

    fn done_at(since: u64) -> activity::AgentState {
        activity::AgentState {
            activity: activity::AgentActivity::Done,
            since_unix: Some(since),
            ..Default::default()
        }
    }

    fn said(notes: &[(String, WorkerNote, Said)]) -> Vec<Said> {
        notes.iter().map(|(_, _, said)| said.clone()).collect()
    }

    #[test]
    fn a_late_card_is_asked_once_and_only_after_the_spawner_had_its_chance() {
        let mut state = WorkerState::default();
        let shells = [
            worker_shell("w", Some("chat"), true, 100),
            worker_shell("orphan", None, true, 0),
        ];
        let none = BTreeMap::new();
        assert!(state.transitions(&shells, &none, "p", 120, true).is_empty());
        let notes = state.transitions(&shells, &none, "p", 130, true);
        assert_eq!(said(&notes), [Said::Late("w".into())]);
        assert_eq!(notes[0].0, "chat");
        assert!(state.transitions(&shells, &none, "p", 500, true).is_empty());
        assert!(
            state
                .transitions(&shells, &none, "other", 500, true)
                .is_empty(),
            "another project's workers are not this window's"
        );
    }

    #[test]
    fn a_done_waits_for_its_last_lines_and_is_said_once() {
        let mut state = WorkerState::default();
        let shells = [worker_shell("w", Some("chat"), true, 1000)];
        let activity = BTreeMap::from([("w".to_owned(), done_at(1010))]);
        assert!(state.poll.begin(0, true, false));
        state.poll.finish(0, false, true, true);
        assert!(!state.poll.has_request());
        assert!(
            state
                .transitions(&shells, &activity, "p", 1011, false)
                .is_empty()
        );
        assert!(state.poll.has_request(), "a sample is asked for");
        state.outputs.insert("w".into(), Ok("Final line".into()));
        state.last_output.insert("w".into(), "Final line".into());
        let notes = state.transitions(&shells, &activity, "p", 1012, true);
        assert_eq!(said(&notes), [Said::Done("w".into(), 1010)]);
        assert!(matches!(
            &notes[0].1,
            WorkerNote::Finish { end, done_since: Some(1010), .. } if end.tail == "Final line"
        ));
        assert!(
            state
                .transitions(&shells, &activity, "p", 1013, true)
                .is_empty()
        );
        // A later done, after more work, is said again.
        let activity = BTreeMap::from([("w".to_owned(), done_at(1100))]);
        state.transitions(&shells, &activity, "p", 1101, false);
        assert_eq!(
            said(&state.transitions(&shells, &activity, "p", 1102, true)),
            [Said::Done("w".into(), 1100)]
        );
    }

    #[test]
    fn a_worker_that_goes_is_recorded_exited_with_its_last_lines_and_retried_without_a_host() {
        let mut state = WorkerState::default();
        let none = BTreeMap::new();
        let alive = [worker_shell("w", Some("chat"), true, 1000)];
        state.transitions(&alive, &none, "p", 1001, true);
        state.last_output.insert("w".into(), "bye".into());
        // Stopped: gone from the inventory.
        let notes = state.transitions(&[], &none, "p", 1002, true);
        assert_eq!(said(&notes), [Said::Exited("w".into())]);
        assert!(matches!(
            &notes[0].1,
            WorkerNote::Finish { end, .. } if end.status == WorkerEnd::Exited && end.tail == "bye"
        ));
        let sent = said(&notes);
        state.settle(&sent, &sent);
        assert_eq!(
            said(&state.transitions(&[], &none, "p", 1003, true)),
            [Said::Exited("w".into())],
            "no host: said again"
        );
        state.settle(&sent, &[]);
        assert!(state.transitions(&[], &none, "p", 1004, true).is_empty());
        assert!(
            !state.seen.contains_key("w"),
            "a recorded exit is no longer watched"
        );
        // A worker that died while no app ran is closed at first sight.
        let dead = [worker_shell("d", Some("chat"), false, 10)];
        assert_eq!(
            said(&state.transitions(&dead, &none, "p", 2000, true)),
            [Said::Exited("d".into())]
        );
        assert!(state.transitions(&dead, &none, "p", 2001, true).is_empty());
    }

    #[test]
    fn a_chat_gets_only_the_workers_it_started() {
        let worker = |status| LiveWorker {
            status,
            actionable: true,
            model: None,
            output: String::new(),
            output_error: None,
        };
        let live = vec![
            (
                "mine".to_owned(),
                "chat-a".to_owned(),
                worker(LiveStatus::Working),
            ),
            (
                "theirs".to_owned(),
                "chat-b".to_owned(),
                worker(LiveStatus::Done),
            ),
            (
                "also-mine".to_owned(),
                "chat-a".to_owned(),
                worker(LiveStatus::Waiting),
            ),
        ];
        assert_eq!(
            workers_of_chat(&live, "chat-a").keys().collect::<Vec<_>>(),
            ["also-mine", "mine"]
        );
        assert!(workers_of_chat(&live, "chat-c").is_empty());
    }

    #[test]
    fn notes_for_a_parent_that_is_no_chat_are_dropped_and_without_a_host_are_kept() {
        let home = std::env::temp_dir().join(format!("riwork-notes-{}", uuid::Uuid::new_v4()));
        let chat = uuid::Uuid::new_v4().to_string();
        let dir = crate::chat::log::chat_dir(&home, &chat).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("info.json"), "{}").unwrap();
        let note = |id: &str| WorkerNote::Spawn {
            session_id: id.into(),
            kind: WorkerKind::Shell,
            late: true,
        };
        let undelivered = deliver_notes(
            &home,
            vec![
                (chat.clone(), note("a"), Said::Late("a".into())),
                ("a-terminal".into(), note("b"), Said::Late("b".into())),
            ],
        );
        assert_eq!(undelivered, [Said::Late("a".into())]);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_done_whose_terminal_could_not_be_read_waits_for_its_last_lines() {
        let mut state = WorkerState::default();
        let shells = [worker_shell("w", Some("chat"), true, 1000)];
        let activity = BTreeMap::from([("w".to_owned(), done_at(1010))]);
        state.last_output.insert("w".into(), "Still working".into());
        assert!(
            state
                .transitions(&shells, &activity, "p", 1011, false)
                .is_empty()
        );
        // The sample went through, but this terminal could not be read.
        state
            .outputs
            .insert("w".into(), Err("capture failed".into()));
        assert!(
            state
                .transitions(&shells, &activity, "p", 1012, true)
                .is_empty(),
            "not recorded with lines from before it was done"
        );
        state.outputs.insert("w".into(), Ok("Final line".into()));
        state.last_output.insert("w".into(), "Final line".into());
        let notes = state.transitions(&shells, &activity, "p", 1013, true);
        assert!(matches!(
            &notes[..],
            [(_, WorkerNote::Finish { end, .. }, _)] if end.tail == "Final line"
        ));
    }

    #[test]
    fn another_projects_sample_says_nothing_about_this_ones_dones() {
        let mut state = WorkerState::default();
        let shells = [worker_shell("w", Some("chat"), true, 1000)];
        let activity = BTreeMap::from([("w".to_owned(), done_at(1010))]);
        assert!(
            state
                .transitions(&shells, &activity, "p", 1011, false)
                .is_empty()
        );
        // The window turns to another project before this one's terminals are read.
        state.last_output.clear();
        state.outputs.clear();
        state.seen.clear();
        assert!(
            state
                .transitions(&[], &activity, "q", 1012, true)
                .is_empty(),
            "nothing is recorded without its last lines"
        );
    }

    #[test]
    fn a_done_said_before_its_late_card_is_said_again_after_it() {
        let mut state = WorkerState::default();
        let shells = [worker_shell("w", Some("chat"), true, 1000)];
        let activity = BTreeMap::from([("w".to_owned(), done_at(1005))]);
        state.last_output.insert("w".into(), "Done early".into());
        state.outputs.insert("w".into(), Ok("Done early".into()));
        // Done before anyone wrote its card: the host had no card to end.
        let notes = state.transitions(&shells, &activity, "p", 1006, true);
        assert_eq!(said(&notes), [Said::Done("w".into(), 1005)]);
        let notes = state.transitions(&shells, &activity, "p", 1031, true);
        assert_eq!(
            said(&notes),
            [Said::Late("w".into()), Said::Done("w".into(), 1005)],
            "the late card, then the done for it"
        );
    }

    /// A host that takes one request and answers it as `answer` says (nothing: it hangs up).
    fn host_answering(home: &Path, answer: Option<&'static str>) -> std::thread::JoinHandle<()> {
        use std::io::{BufRead, Write};
        let socket = crate::chat::client::socket_path(home);
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(error) = answer {
                let id = serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"].clone();
                let mut stream = stream;
                writeln!(
                    stream,
                    "{}",
                    serde_json::json!({"id": id, "ok": false, "error": error})
                )
                .unwrap();
            }
        })
    }

    #[test]
    fn a_note_whose_exchange_broke_is_said_again_and_a_refused_one_is_not() {
        let home =
            std::env::temp_dir().join(format!("rw-{}", &uuid::Uuid::new_v4().to_string()[..8]));
        let chat = uuid::Uuid::new_v4().to_string();
        let dir = crate::chat::log::chat_dir(&home, &chat).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("info.json"), "{}").unwrap();
        let note = || {
            vec![(
                chat.clone(),
                WorkerNote::Spawn {
                    session_id: "w".into(),
                    kind: WorkerKind::Shell,
                    late: true,
                },
                Said::Late("w".into()),
            )]
        };
        let host = host_answering(&home, None);
        assert_eq!(deliver_notes(&home, note()), [Said::Late("w".into())]);
        host.join().unwrap();
        std::fs::remove_file(crate::chat::client::socket_path(&home)).unwrap();
        let host = host_answering(&home, Some("worker w has no card in chat"));
        assert!(
            deliver_notes(&home, note()).is_empty(),
            "a refusal is final"
        );
        host.join().unwrap();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_note_that_found_no_host_keeps_checks_going_with_no_live_terminal() {
        let mut state = WorkerState::default();
        assert!(state.poll.begin(0, false, false));
        state.poll.finish(0, false, true, false);
        let sent = [Said::Exited("w".into())];
        state.settle(&sent, &sent);
        assert!(
            state.poll.begin(60, state.retrying(), false),
            "a later check says it again"
        );
    }

    #[test]
    fn a_done_whose_terminal_went_before_it_was_read_is_its_exit_and_asks_for_nothing_more() {
        let mut state = WorkerState::default();
        let shells = [worker_shell("w", Some("chat"), true, 1000)];
        let activity = BTreeMap::from([("w".to_owned(), done_at(1010))]);
        state.transitions(&shells, &activity, "p", 1001, true);
        state.last_output.insert("w".into(), "Last lines".into());
        assert!(
            state
                .transitions(&shells, &activity, "p", 1011, false)
                .is_empty()
        );
        assert!(state.poll.begin(0, true, false));
        state.poll.finish(0, false, true, true);
        // `riwork shell close` before its terminal was read again.
        let notes = state.transitions(&[], &activity, "p", 1012, true);
        assert_eq!(said(&notes), [Said::Exited("w".into())]);
        assert!(state.awaiting.is_empty());
        assert!(
            !state.poll.has_request(),
            "no check is asked for at once, again and again"
        );
    }

    #[test]
    fn a_note_that_found_no_host_is_retried_whatever_a_later_batch_did() {
        let mut state = WorkerState::default();
        let exited = [Said::Exited("w".into())];
        let late = [Said::Late("v".into())];
        state.settle(&exited, &exited);
        state.settle(&late, &[]);
        assert!(state.retrying(), "the exit is still to be said");
        state.settle(&exited, &[]);
        assert!(!state.retrying());
    }

    #[test]
    fn watcher_only_wakes_for_inventory_activity_or_done_delivery() {
        let home = Path::new("/synthetic/riwork");
        assert!(relevant(&home.join("sessions.json"), home));
        assert!(relevant(&home.join("orchestrator-wakes.json"), home));
        assert!(relevant(&home.join("agent-activity/id.json"), home));
        assert!(!relevant(&home.join("recency.json"), home));
        assert!(!relevant(&home.join("chats/id/events.jsonl"), home));
    }
    struct RecordingOps {
        calls: std::sync::Mutex<Vec<String>>,
        project: String,
    }
    impl WorkerOperations for RecordingOps {
        fn get_worker(&self, _: &str) -> Result<ShellSession, String> {
            let mut shell: ShellSession = serde_json::from_value(serde_json::json!({"id":"fixture","parent_id":"parent","project_id":self.project,"kind":"project","cwd":"/fixture","command":null,"created_at_unix":1})).unwrap();
            shell.alive = true;
            Ok(shell)
        }
        fn send_existing(&self, id: &str, text: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("send:{id}:{text}"));
            Err("Harness is waiting for folder trust; input was not sent".into())
        }
        fn close_existing(&self, id: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("close:{id}"));
            Ok(())
        }
    }
    #[test]
    fn existing_send_refusal_is_preserved_without_retry_and_stop_uses_close() {
        let ops = RecordingOps {
            calls: Default::default(),
            project: "p".into(),
        };
        assert_eq!(
            dispatch(
                &ops,
                "p",
                "fixture",
                &ShellAction::Send("literal 🦀".into())
            )
            .unwrap_err(),
            "Harness is waiting for folder trust; input was not sent"
        );
        dispatch(&ops, "p", "fixture", &ShellAction::Stop).unwrap();
        assert_eq!(
            *ops.calls.lock().unwrap(),
            ["send:fixture:literal 🦀", "close:fixture"]
        );
    }
    #[test]
    fn stale_project_ownership_refuses_both_actions_before_input_or_stop() {
        let ops = RecordingOps {
            calls: Default::default(),
            project: "other".into(),
        };
        for action in [ShellAction::Send("message".into()), ShellAction::Stop] {
            assert!(dispatch(&ops, "p", "fixture", &action).is_err());
        }
        assert!(ops.calls.lock().unwrap().is_empty());
    }
}
