//! Read-only worker cards and the desktop shell sampler's adaptive schedule.
//! Status is provider activity plus tmux liveness, never inferred from output.
use crate::{
    activity::{AgentActivity, AgentState},
    sessions::ShellSession,
    store::Task,
};
use std::time::Duration;

pub const OUTPUT_LINES: usize = 6;
pub const OUTPUT_CHARS_PER_LINE: usize = 240;
pub const OUTPUT_BYTES: usize = 2048;
pub const MAX_INTERVAL: Duration = Duration::from_secs(30);

/// Pure schedule: 2, 4, 8, 16, 30 seconds; no recurring checks without live shells.
pub fn poll_interval(unchanged: u32, live: bool, paused: bool) -> Option<Duration> {
    (live && !paused)
        .then(|| Duration::from_secs((2u64 << unchanged.min(4)).min(MAX_INTERVAL.as_secs())))
}

#[derive(Clone, Debug)]
pub struct PollState {
    pub last_checked: Option<u64>,
    pub next_check: Option<u64>,
    pub unchanged: u32,
    pub checking: bool,
    requested: bool,
    retry: bool,
}
impl Default for PollState {
    fn default() -> Self {
        Self {
            last_checked: None,
            next_check: None,
            unchanged: 0,
            checking: false,
            requested: true,
            retry: false,
        }
    }
}
impl PollState {
    pub fn has_request(&self) -> bool {
        self.requested
    }
    /// A change brings the next check forward; it never puts off one that is due sooner.
    pub fn note_change(&mut self, now: u64, live: bool) {
        self.unchanged = 0;
        let soon =
            poll_interval(0, live, false).map(|interval| now.saturating_add(interval.as_secs()));
        self.next_check = match (self.next_check, soon) {
            (Some(due), Some(soon)) => Some(due.min(soon)),
            (_, soon) => soon,
        };
    }
    /// Events arriving during a read remain pending after that read finishes.
    pub fn request(&mut self) {
        self.requested = true;
        self.unchanged = 0;
    }
    pub fn begin(&mut self, now: u64, live: bool, paused: bool) -> bool {
        if paused || self.checking {
            return false;
        }
        if !self.requested
            && ((!live && !self.retry) || self.next_check.is_some_and(|due| now < due))
        {
            return false;
        }
        self.checking = true;
        self.requested = false;
        true
    }
    pub fn finish(&mut self, now: u64, changed: bool, succeeded: bool, live: bool) {
        self.checking = false;
        self.retry = !succeeded;
        if succeeded {
            self.last_checked = Some(now);
        }
        self.unchanged = if changed || self.requested {
            0
        } else {
            self.unchanged.saturating_add(1)
        };
        self.next_check = poll_interval(self.unchanged, live || !succeeded, false)
            .map(|interval| now.saturating_add(interval.as_secs()));
    }
    pub fn label(&self, now: u64, live: bool, paused: bool, error: Option<&str>) -> String {
        let age = self.last_checked.map_or_else(
            || "not checked yet".to_owned(),
            |at| format!("last checked {}s ago", now.saturating_sub(at)),
        );
        if paused {
            return format!("Paused (window hidden or app backgrounded) · {age}");
        }
        if self.checking {
            return format!("Checking… · {age}");
        }
        if let Some(error) = error {
            return format!(
                "Check failed · backing off · {age} · next check in {}s · {}",
                self.next_check.map_or(0, |at| at.saturating_sub(now)),
                bounded_line(error)
            );
        }
        if !live {
            return format!("Idle · no live shells · {age}");
        }
        let next = self.next_check.map_or(0, |at| at.saturating_sub(now));
        format!(
            "{} · {age} · next check in {next}s",
            if self.unchanged == 0 {
                "Live"
            } else {
                "Backing off"
            }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskLink {
    pub id: String,
    pub title: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellCard {
    pub id: String,
    pub harness: String,
    /// The registry does not persist a shell's active model. Keep it unknown.
    pub model: Option<String>,
    pub tasks: Vec<TaskLink>,
    pub activity: AgentActivity,
    pub alive: bool,
    pub created_at: u64,
    pub ended_at: Option<u64>,
    pub output: String,
    pub output_error: Option<String>,
}
impl ShellCard {
    pub fn of(shell: &ShellSession, state: Option<&AgentState>, tasks: &[Task]) -> Self {
        let activity = if !shell.alive {
            AgentActivity::Exited
        } else {
            state.map_or(AgentActivity::Unknown, |s| s.activity)
        };
        Self {
            id: shell.id.clone(),
            harness: shell.harness.map_or("Shell", |h| h.program()).into(),
            model: None,
            tasks: tasks
                .iter()
                .filter(|task| {
                    Some(&task.project_id) == shell.project_id.as_ref()
                        && shell.worktree_id.is_some()
                        && task.worktree_id == shell.worktree_id
                })
                .map(|task| TaskLink {
                    id: task.id.clone(),
                    title: bounded_line(&task.title),
                })
                .collect(),
            activity,
            alive: shell.alive,
            created_at: shell.created_at_unix,
            ended_at: (activity == AgentActivity::Done)
                .then(|| state.and_then(|s| s.since_unix))
                .flatten(),
            output: String::new(),
            output_error: None,
        }
    }
    pub fn finished(&self) -> bool {
        matches!(self.activity, AgentActivity::Done | AgentActivity::Exited)
    }
    pub fn elapsed(&self, now: u64) -> u64 {
        self.ended_at.unwrap_or(now).saturating_sub(self.created_at)
    }
    pub fn status(&self) -> &'static str {
        match self.activity {
            AgentActivity::Working => "Working",
            AgentActivity::Waiting => "Waiting for input",
            AgentActivity::Done => "Done",
            AgentActivity::Exited => "Exited",
            AgentActivity::Unknown => "Unknown",
        }
    }
    pub fn title(&self) -> String {
        self.tasks.first().map_or_else(
            || {
                format!(
                    "{} worker · {}",
                    self.harness,
                    self.id.chars().take(8).collect::<String>()
                )
            },
            |task| {
                if self.tasks.len() == 1 {
                    task.title.clone()
                } else {
                    format!("{} · +{} tasks", task.title, self.tasks.len() - 1)
                }
            },
        )
    }
}
fn bounded_line(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(OUTPUT_CHARS_PER_LINE)
        .collect()
}
/// Last nonblank output, bounded independently by lines, characters and UTF-8 bytes.
pub fn bounded_output(text: &str) -> String {
    let lines: Vec<_> = text
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(OUTPUT_LINES)
        .map(bounded_line)
        .collect();
    let mut output = lines.into_iter().rev().collect::<Vec<_>>().join("\n");
    if output.len() > OUTPUT_BYTES {
        let mut start = output.len() - OUTPUT_BYTES;
        while !output.is_char_boundary(start) {
            start += 1;
        }
        output.drain(..start);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn schedule_caps_and_stops_while_hidden_or_without_live_shells() {
        assert_eq!(
            (0..7)
                .map(|n| poll_interval(n, true, false).unwrap().as_secs())
                .collect::<Vec<_>>(),
            [2, 4, 8, 16, 30, 30, 30]
        );
        assert_eq!(poll_interval(u32::MAX, true, false), Some(MAX_INTERVAL));
        assert_eq!(poll_interval(0, false, false), None);
        assert_eq!(poll_interval(0, true, true), None);
    }
    #[test]
    fn five_quiet_minutes_cost_fourteen_checks_instead_of_one_hundred_fifty_one() {
        let mut state = PollState::default();
        let mut checks = Vec::new();
        for now in 0..=300 {
            if state.begin(now, true, false) {
                checks.push(now);
                state.finish(now, now == 0, true, true);
            }
        }
        assert_eq!(&checks[..6], &[0, 2, 6, 14, 30, 60]);
        assert_eq!(checks.len(), 14);
        assert_eq!((0..=300).step_by(2).count(), 151);
        state.request();
        assert!(state.begin(301, true, false), "known events bypass backoff");
    }
    #[test]
    fn events_during_reads_and_pauses_are_not_lost_and_errors_are_not_freshness() {
        let mut state = PollState::default();
        assert!(!state.begin(0, true, true));
        assert!(state.begin(1, true, false));
        state.request();
        assert!(!state.begin(1, true, false));
        state.finish(1, false, true, true);
        assert!(state.begin(1, true, false));
        state.finish(2, false, false, true);
        assert_eq!(state.last_checked, Some(1));
        assert!(
            state
                .label(3, true, false, Some("offline"))
                .contains("last checked 2s ago")
        );
        assert!(state.label(3, true, true, None).starts_with("Paused"));
        state.request();
        assert!(state.begin(3, false, false));
        state.finish(3, true, true, false);
        assert!(
            !state.begin(1000, false, false),
            "no recurring no-shell polling"
        );
    }
    fn shell() -> ShellSession {
        let mut shell: ShellSession = serde_json::from_value(json!({"id":"shell","project_id":"p","worktree_id":"w","kind":"project","cwd":"/fixture","command":null,"harness":"claude","created_at_unix":10})).unwrap();
        shell.alive = true;
        shell
    }
    #[test]
    fn statuses_are_real_and_exit_is_not_invented_failure() {
        let mut shell = shell();
        for (activity, expected, finished) in [
            (AgentActivity::Working, "Working", false),
            (AgentActivity::Waiting, "Waiting for input", false),
            (AgentActivity::Done, "Done", true),
            (AgentActivity::Unknown, "Unknown", false),
        ] {
            let card = ShellCard::of(
                &shell,
                Some(&AgentState {
                    activity,
                    since_unix: Some(20),
                    ..Default::default()
                }),
                &[],
            );
            assert_eq!(card.status(), expected);
            assert_eq!(card.finished(), finished);
            assert_eq!(card.elapsed(30), if finished { 10 } else { 20 });
            assert_eq!(card.model, None);
        }
        shell.alive = false;
        assert_eq!(
            ShellCard::of(
                &shell,
                Some(&AgentState::plain(AgentActivity::Working)),
                &[]
            )
            .status(),
            "Exited"
        );
    }
    #[test]
    fn linkage_requires_project_and_nonempty_worktree_and_keeps_ambiguous_tasks() {
        let shell = shell();
        let task = |id: &str, project: &str, worktree: Option<&str>| Task {
            id: id.into(),
            project_id: project.into(),
            worktree_id: worktree.map(str::to_owned),
            title: id.into(),
            details: String::new(),
            status: Default::default(),
            created_at: 0,
            updated_at: 0,
        };
        let tasks = vec![
            task("a", "p", Some("w")),
            task("b", "p", Some("w")),
            task("foreign", "other", Some("w")),
            task("unlinked", "p", None),
        ];
        let card = ShellCard::of(&shell, None, &tasks);
        assert_eq!(
            card.tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(card.title(), "a · +1 tasks");
        let mut unlinked = shell;
        unlinked.worktree_id = None;
        assert!(ShellCard::of(&unlinked, None, &tasks).tasks.is_empty());
    }
    #[test]
    fn output_is_tail_bounded_even_for_long_unicode_lines_and_control_characters() {
        let output = bounded_output(
            &(0..20)
                .map(|n| format!("line {n} {}\u{7}\n", "🦀".repeat(300)))
                .collect::<String>(),
        );
        assert!(output.len() <= OUTPUT_BYTES);
        assert!(output.lines().count() <= OUTPUT_LINES);
        assert!(output.ends_with('🦀'));
        assert!(output.contains("line 19"));
        assert!(!output.contains("line 13"));
        assert!(!output.contains('\u{7}'));
        assert_eq!(bounded_output("a\nb\n\n\n"), "a\nb");
    }
    #[test]
    fn failed_discovery_retries_without_reporting_a_fresh_check_or_no_shells() {
        let mut state = PollState::default();
        assert!(state.begin(0, false, false));
        state.finish(0, false, false, false);
        assert_eq!(state.last_checked, None);
        assert!(!state.begin(3, false, false));
        assert!(state.begin(4, false, false));
        assert!(
            state
                .label(4, false, false, Some("offline"))
                .starts_with("Checking")
        );
        state.finish(4, true, true, false);
        assert!(!state.begin(100, false, false));
    }
    #[test]
    fn a_real_change_restores_fast_checks_and_pausing_keeps_the_last_success_age() {
        let mut state = PollState::default();
        for now in [0, 4, 12, 28] {
            assert!(state.begin(now, true, false));
            state.finish(now, false, true, true);
        }
        assert_eq!(state.next_check, Some(58));
        state.note_change(30, true);
        assert_eq!(state.next_check, Some(32));
        assert!(
            state
                .label(31, true, true, None)
                .contains("last checked 3s ago")
        );
        assert!(!state.begin(32, true, true));
        state.request();
        assert!(state.begin(33, true, false));
    }
    #[test]
    fn steady_changes_never_postpone_a_check_that_is_due() {
        let mut state = PollState::default();
        assert!(state.begin(0, true, false));
        state.finish(0, false, true, true);
        assert_eq!(state.next_check, Some(4));
        let mut checks = Vec::new();
        for now in 1..=10 {
            state.note_change(now, true);
            if state.begin(now, true, false) {
                checks.push(now);
                state.finish(now, true, true, true);
            }
        }
        assert_eq!(checks, [3, 5, 7, 9]);
    }
}
