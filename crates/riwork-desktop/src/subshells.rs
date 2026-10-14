//! Workspace integration: reuse the inventory sample and the existing shell operations.
use crate::{
    Workspace, activity,
    chat_view::ShellAction,
    sessions::{SessionManager, ShellSession},
};
use gpui::{Context, Window};
use notify::{RecursiveMode, Watcher};
use riwork_engine::subshells::{PollState, ShellCard, bounded_output};
use std::{collections::BTreeMap, path::Path, time::Instant};

pub(crate) struct WorkerState {
    pub poll: PollState,
    pub models: BTreeMap<String, String>,
    clock: Instant,
    scope: String,
    outputs: BTreeMap<String, Result<String, String>>,
    pub cards: Vec<ShellCard>,
    retired: Vec<ShellCard>,
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
            cards: Vec::new(),
            retired: Vec::new(),
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
            self.subshells.cards.clear();
            self.subshells.retired.clear();
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
        let live = self.worker_sample_live();
        let sample =
            self.subshells
                .poll
                .begin(self.subshells.clock.elapsed().as_secs(), live, paused);
        self.publish_workers(cx);
        sample
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
        self.subshells.outputs.extend(outputs);
        self.subshells.poll.finish(
            self.subshells.clock.elapsed().as_secs(),
            changed || output_changed,
            succeeded,
            self.worker_sample_live(),
        );
        self.publish_workers(cx);
    }

    pub(crate) fn publish_workers(&mut self, cx: &mut Context<Self>) {
        self.subshells
            .outputs
            .retain(|id, _| self.shells.iter().any(|shell| &shell.id == id));
        let mut cards: Vec<_> = self
            .shells
            .iter()
            .filter(|s| s.is_worker() && s.project_id.as_deref() == Some(self.project_id.as_str()))
            .map(|shell| {
                let mut card =
                    ShellCard::of(shell, self.agent_activity.get(&shell.id), &self.state.tasks);
                card.model = self.subshells.models.get(&shell.id).cloned();
                if let Some(output) = self.subshells.outputs.get(&shell.id) {
                    match output {
                        Ok(text) => card.output = text.clone(),
                        Err(error) => {
                            card.output_error = Some(bounded_output(error));
                            card.output = self
                                .subshells
                                .cards
                                .iter()
                                .find(|old| old.id == shell.id)
                                .map(|old| old.output.clone())
                                .unwrap_or_default();
                        }
                    }
                }
                card
            })
            .collect();
        cards.extend(
            self.subshells
                .retired
                .iter()
                .filter(|old| !cards.iter().any(|card| card.id == old.id))
                .cloned()
                .collect::<Vec<_>>(),
        );
        cards.sort_by_key(|card| (card.finished(), card.created_at));
        self.subshells.cards = cards.clone();
        let mut poll = self.subshells.poll.label(
            self.subshells.clock.elapsed().as_secs(),
            self.worker_sample_live(),
            self.subshells.paused,
            self.sessions_sample_error.as_deref(),
        );
        if let Some(error) = &self.subshells.watch_error {
            poll.push_str(" · ");
            poll.push_str(error);
        }
        let project = self.project_id.clone();
        for view in self
            .panes
            .values()
            .flat_map(|p| p.tabs.iter())
            .filter_map(|tab| tab.chat())
            .cloned()
            .collect::<Vec<_>>()
        {
            let belongs = view.read(cx).summary().project_id.as_deref() == Some(project.as_str());
            view.update(cx, |view, cx| {
                view.set_subshells(
                    if belongs { cards.clone() } else { Vec::new() },
                    poll.clone(),
                    cx,
                )
            });
        }
    }

    pub(crate) fn worker_activity_changed(&mut self, cx: &mut Context<Self>) {
        self.subshells.poll.note_change(
            self.subshells.clock.elapsed().as_secs(),
            self.worker_sample_live(),
        );
        self.publish_workers(cx);
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
        let stop = matches!(action, ShellAction::Stop);
        let view = view.downgrade();
        let work_id = id.clone();
        let work_project = project.clone();
        let work = cx
            .background_executor()
            .spawn(async move { dispatch(&sessions, &work_project, &work_id, &action) });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |workspace, window, cx| {
                if result.is_ok() && stop && workspace.project_id == project {
                    if let Some(mut card) = workspace
                        .subshells
                        .cards
                        .iter()
                        .find(|card| card.id == id)
                        .cloned()
                    {
                        card.alive = false;
                        card.activity = activity::AgentActivity::Exited;
                        card.ended_at = Some(activity::unix_now());
                        workspace.subshells.retired.push(card);
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;
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
